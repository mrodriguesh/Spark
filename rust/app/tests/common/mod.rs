//! Shared by the engine tests: the fake driver of test/engine-driver.js (`fakeTinder`) ported to Rust, a
//! recording wrapper and a seeded Env that records sleeps.
#![allow(dead_code)]
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use serde_json::{Value, json};
use spark::{
    core::{
        engine::{Driver, Env, Mulberry32, RoundOpts},
        js::num,
    },
    iface::Fut,
};

/// `fakeTinder(o)` options (absent = undefined).
#[derive(Clone, Debug, Default)]
pub struct FakeOpts {
    pub limit: Option<i64>,
    pub offer: bool,
    pub match_at: Option<i64>,
    pub empty_after: Option<i64>,
    pub loading: i64,
    pub freeze: bool,
    pub login: bool,
}

impl FakeOpts {
    pub fn from_json(o: &Value) -> Self {
        let int = |k: &str| spark::core::js::number(&o[k]).map(|f| f as i64);
        FakeOpts {
            limit: int("limit"),
            offer: o["offer"] == Value::Bool(true),
            match_at: int("matchAt"),
            empty_after: int("emptyAfter"),
            loading: int("loading").unwrap_or(0),
            freeze: o["freeze"] == Value::Bool(true),
            login: o["login"] == Value::Bool(true),
        }
    }
}

/// `t`: the fake's state.
#[derive(Debug)]
pub struct T {
    pub screen: &'static str,
    pub i: i64,
    pub likes: i64,
    pub nopes: i64,
    pub swipes: Vec<&'static str>,
    pub dismissed: i64,
    pub reloads: i64,
    pub observed: i64,
    pub gone: bool,
    pub loading: i64,
}

pub struct FakeTinder {
    pub t: RefCell<T>,
    o: FakeOpts,
    /// test/engine-driver.js `d.dismiss = async () => { d.t.dismissed++ }`: a popup that never closes.
    pub stubborn: Cell<bool>,
}

fn text(screen: &str) -> &'static str {
    match screen {
        "out" => "You're out of Likes",
        "offer" => "Curtidas ilimitadas. Escolha um plano",
        "match" => "It's a Match!",
        "popup" => "Add to Home screen?",
        "empty" => "There's no one new around you",
        "login" => "Log in",
        _ => "",
    }
}

impl FakeTinder {
    pub fn new(o: FakeOpts) -> Self {
        let t = T {
            screen: if o.login { "login" } else { "card" },
            i: 1,
            likes: 0,
            nopes: 0,
            swipes: vec![],
            dismissed: 0,
            reloads: 0,
            observed: 0,
            gone: false,
            loading: o.loading,
        };
        FakeTinder {
            t: RefCell::new(t),
            o,
            stubborn: Cell::new(false),
        }
    }

    fn look(&self) -> Value {
        let mut t = self.t.borrow_mut();
        t.observed += 1;
        let loading = t.screen == "card" && t.loading > 0;
        if loading {
            t.loading -= 1;
        }
        let tx = text(t.screen);
        let snap = if t.screen == "card" {
            if loading {
                String::new()
            } else {
                format!("Pessoa {}", t.i)
            }
        } else {
            tx.to_string()
        };
        json!({ "url": "fake://tinder", "text": tx, "full": tx, "snap": snap, "ready": t.screen == "card" && !loading,
            "dialog": t.screen == "popup" || t.screen == "match", "login": t.screen == "login" })
    }
}

impl Driver for FakeTinder {
    fn observe(&self) -> Fut<'_, Result<Value, String>> {
        Box::pin(async move { Ok(self.look()) })
    }
    fn settle<'a>(&'a self, last: &'a Value, _ms: Option<f64>) -> Fut<'a, Result<Value, String>> {
        Box::pin(async move {
            let x = self.look();
            Ok(Value::Bool(if last.is_null() {
                x["ready"] == Value::Bool(true)
            } else {
                x["snap"] != *last
            }))
        })
    }
    fn swipe(&self, right: bool) -> Fut<'_, Result<(), String>> {
        Box::pin(async move {
            let mut t = self.t.borrow_mut();
            t.swipes.push(if right { "like" } else { "nope" });
            if self.o.freeze {
                return Ok(());
            }
            if right {
                // like the real Tinder: the limit shows on the try after the last accepted like (that tap is NOT a like)
                if t.likes >= self.o.limit.unwrap_or(5) {
                    t.screen = if self.o.offer { "offer" } else { "out" };
                    return Ok(());
                }
                t.likes += 1;
                if Some(t.likes) == self.o.match_at {
                    t.screen = "match";
                }
            } else {
                t.nopes += 1;
                if self.o.empty_after.is_some_and(|n| n != 0 && t.nopes == n) {
                    t.screen = "empty";
                }
            }
            t.i += 1;
            Ok(())
        })
    }
    fn dismiss(&self) -> Fut<'_, Result<(), String>> {
        Box::pin(async move {
            let mut t = self.t.borrow_mut();
            t.dismissed += 1;
            if !self.stubborn.get() {
                t.screen = "card";
            }
            Ok(())
        })
    }
    fn gone(&self) -> bool {
        self.t.borrow().gone
    }
    fn reload(&self) -> Fut<'_, ()> {
        Box::pin(async move {
            let mut t = self.t.borrow_mut();
            t.reloads += 1;
            t.loading = self.o.loading;
        })
    }
    fn logged_in(&self) -> Fut<'_, Result<bool, String>> {
        Box::pin(async move { Ok(self.t.borrow().screen != "login") })
    }
}

pub type Calls = Rc<RefCell<Vec<Value>>>;

/// Records every call round() makes, in the oracle's shape.
pub struct Recorder<'a> {
    pub d: &'a dyn Driver,
    pub calls: Calls,
}

impl Driver for Recorder<'_> {
    fn observe(&self) -> Fut<'_, Result<Value, String>> {
        Box::pin(async move {
            let p = self.d.observe().await?;
            self.calls.borrow_mut().push(json!(["observe", p]));
            Ok(p)
        })
    }
    fn settle<'a>(&'a self, last: &'a Value, ms: Option<f64>) -> Fut<'a, Result<Value, String>> {
        Box::pin(async move {
            let r = self.d.settle(last, ms).await?;
            self.calls.borrow_mut().push(json!(["settle", last, r]));
            Ok(r)
        })
    }
    fn swipe(&self, right: bool) -> Fut<'_, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(json!(["swipe", right]));
            self.d.swipe(right).await
        })
    }
    fn dismiss(&self) -> Fut<'_, Result<(), String>> {
        Box::pin(async move {
            self.calls.borrow_mut().push(json!(["dismiss"]));
            self.d.dismiss().await
        })
    }
    fn gone(&self) -> bool {
        let g = self.d.gone();
        self.calls.borrow_mut().push(json!(["gone", g]));
        g
    }
    fn reload(&self) -> Fut<'_, ()> {
        Box::pin(async move {
            self.calls.borrow_mut().push(json!(["reload"]));
            self.d.reload().await
        })
    }
    fn logged_in(&self) -> Fut<'_, Result<bool, String>> {
        self.d.logged_in()
    }
}

/// Seeded `Math.random`; sleeps resolve at once and are recorded (when `calls` is set).
pub struct TestEnv {
    pub rng: Mulberry32,
    pub calls: Option<Calls>,
}

impl Env for TestEnv {
    fn random(&self) -> f64 {
        self.rng.next_f64()
    }
    fn sleep(&self, ms: f64) -> Fut<'_, ()> {
        if let Some(c) = &self.calls {
            c.borrow_mut().push(json!(["sleep", num(ms)]));
        }
        Box::pin(async {})
    }
}

pub fn env(seed: u32) -> TestEnv {
    TestEnv {
        rng: Mulberry32::new(seed),
        calls: None,
    }
}

/// test/engine-driver.js `fast(extra)`: delay [0, 1], never stops, no callbacks.
pub fn fast() -> RoundOpts {
    RoundOpts {
        skip_first: false,
        per_load: None,
        delay: [0.0, 1.0],
        stop: Box::new(|| false),
        on_like: Box::new(|_| {}),
        on_match: Box::new(|| {}),
    }
}
