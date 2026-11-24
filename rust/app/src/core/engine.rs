//! engine.js: `classify`/`state`, `round` (S-06.2) and `autostartPlan`, plus the driver contract (S-06.3).
//! `classify` and `autostartPlan` are the recorded `spark-classify` unit; `round` is a hand port held to the
//!.
use std::cell::Cell;

use serde_json::{Value, json};
use spark_classify::{js::is_undefined, port::plain};

use super::js;
use crate::iface::{Fut, LoginItem};

/// `STUCK_AFTER`: swipes in a row without the screen changing.
pub const STUCK_AFTER: i64 = 6;

/// What the engine needs from a page (driver-web.js:1-8).
pub trait Driver {
    /// `{ url, text, full, snap, ready, dialog, login }`.
    fn observe(&self) -> Fut<'_, Result<Value, String>>;
    /// Wait until the card is ready (`last` null) or changed from `last`; `ms` None = the default 500.
    fn settle<'a>(&'a self, last: &'a Value, ms: Option<f64>) -> Fut<'a, Result<Value, String>>;
    fn swipe(&self, right: bool) -> Fut<'_, Result<(), String>>;
    fn dismiss(&self) -> Fut<'_, Result<(), String>>;
    fn gone(&self) -> bool;
    /// Never fails (driver-web swallows the rejection).
    fn reload(&self) -> Fut<'_, ()>;
    fn logged_in(&self) -> Fut<'_, Result<bool, String>>;
}

/// The randomness and timers round() uses: `Math.random()` and `sleep(ms)`.
pub trait Env {
    fn random(&self) -> f64;
    fn sleep(&self, ms: f64) -> Fut<'_, ()>;
}

/// mulberry32: uniform in [0, 1) with 2^-32 steps; the traces seed it, production seeds it from the clock.
pub struct Mulberry32(Cell<u32>);

impl Mulberry32 {
    pub fn new(seed: u32) -> Self {
        Mulberry32(Cell::new(seed))
    }

    pub fn next_f64(&self) -> f64 {
        let a = self.0.get().wrapping_add(0x6D2B_79F5);
        self.0.set(a);
        let mut t = (a ^ (a >> 15)).wrapping_mul(1 | a);
        t = t.wrapping_add((t ^ (t >> 7)).wrapping_mul(61 | t)) ^ t;
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

/// `setTimeout`-based sleep of a JS ms delay (negative = 0).
pub async fn sleep_ms(ms: f64) {
    let ns = if ms > 0.0 { (ms * 1e6) as u64 } else { 0 };
    tokio::time::sleep(std::time::Duration::from_nanos(ns)).await;
}

/// Production Env: mulberry32 seeded from the system clock, tokio timers.
pub struct RealEnv(Mulberry32);

impl RealEnv {
    pub fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        RealEnv(Mulberry32::new((nanos as u32) ^ ((nanos >> 32) as u32)))
    }
}

impl Default for RealEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl Env for RealEnv {
    fn random(&self) -> f64 {
        self.0.next_f64()
    }
    fn sleep(&self, ms: f64) -> Fut<'_, ()> {
        Box::pin(sleep_ms(ms))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Out,
    Empty,
    Match,
    Popup,
    Ok,
}

/// `classify(p)`: `{ s, snap, ready, url, login }`; snap/ready/url as the page gave them (absent = the unit's
/// `undefined` marker).
#[derive(Clone, Debug, PartialEq)]
pub struct Classified {
    pub s: Screen,
    pub snap: Value,
    pub ready: Value,
    pub url: Value,
    pub login: bool,
}

pub fn classify(p: &Value) -> Result<Classified, String> {
    let c = plain::classify_value(p).map_err(|e| js::message(&e))?;
    let s = match c["s"].as_str() {
        Some("out") => Screen::Out,
        Some("empty") => Screen::Empty,
        Some("match") => Screen::Match,
        Some("popup") => Screen::Popup,
        _ => Screen::Ok,
    };
    Ok(Classified {
        s,
        snap: c["snap"].clone(),
        ready: c["ready"].clone(),
        url: c["url"].clone(),
        login: c["login"] == Value::Bool(true),
    })
}

/// `state(driver)`.
pub async fn state(d: &dyn Driver) -> Result<Classified, String> {
    classify(&d.observe().await?)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Reload,
    Out,
    Empty,
    Stuck,
    Login,
    Stopped,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Reload => "reload",
            Reason::Out => "out",
            Reason::Empty => "empty",
            Reason::Stuck => "stuck",
            Reason::Login => "login",
            Reason::Stopped => "stopped",
        }
    }
}

/// `{ reason, likes }`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoundResult {
    pub reason: Reason,
    pub likes: i64,
}

impl RoundResult {
    pub fn to_json(self) -> Value {
        json!({ "reason": self.reason.as_str(), "likes": self.likes })
    }
}

/// round()'s `opts`: `perLoad` None (or 0) = no limit per load.
pub struct RoundOpts {
    pub skip_first: bool,
    pub per_load: Option<i64>,
    pub delay: [f64; 2],
    pub stop: Box<dyn Fn() -> bool>,
    pub on_like: Box<dyn Fn(i64)>,
    pub on_match: Box<dyn Fn()>,
}

/// `!snap.trim()` with the TypeError a non-string snap throws.
fn blank(snap: &Value) -> Result<bool, String> {
    match snap {
        Value::String(s) => Ok(js::trim(s).is_empty()),
        Value::Null => Err("Cannot read properties of null (reading 'trim')".into()),
        v if is_undefined(v) => Err("Cannot read properties of undefined (reading 'trim')".into()),
        _ => Err("snap.trim is not a function".into()),
    }
}

/// engine.js:29-75. Likes are reported (onLike) only once confirmed, as positive deltas.
pub async fn round(d: &dyn Driver, opts: &RoundOpts, env: &dyn Env) -> Result<RoundResult, String> {
    let (mut swipes, mut stuck, mut popups, mut loading, mut waits) =
        (0i64, 0i64, 0i64, 0i64, 0i64);
    let likes = Cell::new(0i64);
    let unconfirmed = Cell::new(0i64);
    let mut last = Value::Null;
    let confirm = |n: i64| {
        if n > 0 {
            likes.set(likes.get() + n);
            (opts.on_like)(n);
        }
        unconfirmed.set(0);
    };
    let end = |reason: Reason, undo: i64| {
        confirm(unconfirmed.get() - undo);
        Ok(RoundResult {
            reason,
            likes: likes.get(),
        })
    };
    loop {
        if (opts.stop)() || d.gone() {
            return end(Reason::Stopped, 0);
        }
        let Classified {
            s,
            snap,
            ready,
            login,
            ..
        } = state(d).await?;
        if login {
            return end(Reason::Login, unconfirmed.get());
        }
        if s == Screen::Out {
            return end(Reason::Out, unconfirmed.get().min(1));
        }
        if s == Screen::Empty {
            return end(Reason::Empty, 0);
        }
        if s == Screen::Match || s == Screen::Popup {
            if s == Screen::Match && popups == 0 {
                (opts.on_match)();
            }
            confirm(unconfirmed.get());
            popups += 1;
            if popups > 8 {
                return end(Reason::Stuck, 0);
            }
            d.dismiss().await?;
            env.sleep(1500.0).await;
            continue;
        }
        popups = 0;
        if let Some(per_load) = opts.per_load.filter(|n| *n != 0)
            && swipes >= per_load
        {
            let same = js::strict_eq(&snap, &last);
            if same {
                waits += 1;
                if waits < 10 {
                    d.settle(&last, None).await?;
                    continue;
                }
            }
            return end(Reason::Reload, if same { unconfirmed.get() } else { 0 });
        }
        if blank(&snap)? || is_undefined(&ready) || !js::truthy(&ready) {
            loading += 1;
            if loading >= 90 {
                return end(Reason::Stuck, 0);
            }
            d.settle(&Value::Null, None).await?;
            continue;
        }
        loading = 0;
        let same = js::strict_eq(&snap, &last);
        if !same {
            confirm(unconfirmed.get());
        }
        stuck = if same { stuck + 1 } else { 0 };
        if stuck >= STUCK_AFTER {
            return end(Reason::Stuck, unconfirmed.get());
        }
        last = snap;

        let right = !(opts.skip_first && swipes == 0);
        d.swipe(right).await?;
        swipes += 1;
        if right {
            unconfirmed.set(unconfirmed.get() + 1);
        }
        let [a, b] = opts.delay;
        env.sleep(a + env.random() * (b - a)).await;
    }
}

/// `autostartPlan(s, wanted)`: `{ autostart, register }` (Windows' entry wins; with none, the saved option).
pub fn autostart_plan(s: &LoginItem, wanted: &Value) -> Result<Value, String> {
    let s = json!({ "openAtLogin": s.open_at_login, "executableWillLaunchAtLogin": s.executable_will_launch_at_login });
    plain::autostart_plan_value(&s, wanted).map_err(|e| js::message(&e))
}
