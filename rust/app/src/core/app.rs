//! main.js on the logic thread: startup (S-01.10), db and log (S-02, S-04), IPC (S-03), the cycle and its waits
//! (S-05), the Tinder page's events (S-07.5-6) and the retry task (S-08). Single-threaded like Node: one
//! `Rc<App>`, short `RefCell` borrows, every JS async function split into its synchronous prefix (run at the
//! call) and a spawned or awaited continuation, timers on tokio time.
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    io::Write,
    path::{Path, PathBuf},
    rc::Rc,
};

use serde_json::{Map, Value, json};
use spark_store::port as store;
use tokio::{
    sync::{Notify, mpsc::UnboundedReceiver},
    task::{AbortHandle, spawn_local},
};

use super::{
    clock::{self, Clock, SystemClock},
    driver_web::{HOME, WebDriver},
    engine::{self, Driver, Env, RealEnv, Reason, RoundOpts, RoundResult, sleep_ms},
    exec::{Exec, ProcessExec},
    geo::{self, GeoCache},
    js::{self, num, truthy},
    retry::{PsRetryTask, RetryTask},
};
use crate::iface::{Fut, Host, HostEvent, Page, Placement};

pub const RETRY_MINUTES: [f64; 5] = [15.0, 30.0, 60.0, 120.0, 360.0];
/// main.js `RETRY`: seconds between attempts without a connection.
pub const OFFLINE_BACKOFF_S: [f64; 4] = [5.0, 15.0, 30.0, 60.0];
pub const RETRY_ARG: &str = "--spark-retry-at=";
pub const HIDDEN_ARG: &str = "--hidden";
pub const LOG_NAME: &str = "spark.log";
const MIN: f64 = 60_000.0;
/// setTimeout's limit; a later deadline re-arms when the timer fires.
const TIMEOUT_MAX: f64 = 2_147_483_647.0;
const SETTABLE: [&str; 4] = ["skipFirst", "autostart", "startMinimized", "retryMin"];

/// engine.round as main.js calls it; tests swap it like agenda.js swaps `engine.round`.
pub type RoundFn =
    Rc<dyn Fn(Rc<dyn Driver>, RoundOpts) -> Fut<'static, Result<RoundResult, String>>>;
/// engine.writeJson.
pub type WriteJson = Rc<dyn Fn(&Path, &Value) -> Result<(), String>>;

/// What the core needs besides the host; `production()` is the real thing, tests inject fakes.
pub struct Deps {
    pub clock: Rc<dyn Clock>,
    pub exec: Rc<dyn Exec>,
    /// Used only when `host.packaged()`; None = the PowerShell task for `current_exe()` and userData.
    pub retry_task: Option<Rc<dyn RetryTask>>,
    pub write_json: WriteJson,
    pub env: Rc<dyn Env>,
    /// None = `engine::round` with `env`.
    pub round: Option<RoundFn>,
}

impl Deps {
    pub fn production() -> Self {
        Deps {
            clock: Rc::new(SystemClock),
            exec: Rc::new(ProcessExec),
            retry_task: None,
            write_json: Rc::new(|f: &Path, v: &Value| store::write_json(f, v)),
            env: Rc::new(RealEnv::new()),
            round: None,
        }
    }
}

#[derive(Clone)]
struct Pending {
    start: f64,
    min: f64,
    why: String,
    code: &'static str,
}

struct Status {
    code: &'static str,
    text: String,
    next: f64,
}

struct State {
    db: Map<String, Value>,
    status: Status,
    logs: VecDeque<String>,
    /// `win` exists (createWindow ran; the host creates the tray with it).
    win: bool,
    page: Option<Rc<dyn Page>>,
    drv: Option<Rc<dyn Driver>>,
    /// `wc.once('did-stop-loading', cycle)` not yet used for the current page.
    stop_once: bool,
    timer: Option<AbortHandle>,
    login_poll: Option<AbortHandle>,
    midnight: Option<AbortHandle>,
    busy: bool,
    fresh: bool,
    offline: bool,
    quitting: bool,
    attempting: bool,
    schedule_version: u64,
    pending: Option<Pending>,
    fails: usize,
}

pub struct App {
    host: Rc<dyn Host>,
    clock: Rc<dyn Clock>,
    write_json: WriteJson,
    round: RoundFn,
    retry: Option<Rc<dyn RetryTask>>,
    file: PathBuf,
    log_file: PathBuf,
    retry_arg: Option<String>,
    scheduled_at: f64,
    unattended: bool,
    hidden: bool,
    load_error: Option<String>,
    recovered_from: Option<String>,
    geo: GeoCache,
    st: RefCell<State>,
    events: RefCell<UnboundedReceiver<HostEvent>>,
    /// `app.quit()` was called: the event loop ends.
    quit: Notify,
}

/// Runs main.js until `app.quit()` (or until the host drops its event sender).
pub async fn run_app(
    deps: Deps,
    argv: Vec<String>,
    user_data: PathBuf,
    host: Rc<dyn Host>,
    events: UnboundedReceiver<HostEvent>,
) {
    let app = App::new(deps, argv, user_data, host, events);
    if app.start().await {
        app.event_loop().await;
    }
    app.shutdown();
}

/// main.js `${String(e.message || e).split('\n')[0]}`.
fn first_line(e: &str) -> &str {
    if e.is_empty() {
        "Error"
    } else {
        e.split('\n').next().unwrap_or(e)
    }
}

/// `JSON.parse(fs.readFileSync(file, 'utf8'))` with Node's messages.
fn read_saved(file: &Path) -> Result<Value, String> {
    let text = match fs::read(file) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "ENOENT: no such file or directory, open '{}'",
                file.display()
            ));
        }
        Err(e) => return Err(e.to_string()),
    };
    store::parse(&text).map_err(|e| js::message(&e))
}

/// `saved.<key>`: `null`/`undefined` throw; a primitive or array has no such own property.
fn prop<'a>(v: &'a Value, key: &str) -> Result<Option<&'a Value>, String> {
    match v {
        Value::Null => Err(format!("Cannot read properties of null (reading '{key}')")),
        Value::Object(o) => Ok(o.get(key)),
        _ => Ok(None),
    }
}

impl App {
    fn new(
        deps: Deps,
        argv: Vec<String>,
        user_data: PathBuf,
        host: Rc<dyn Host>,
        events: UnboundedReceiver<HostEvent>,
    ) -> Rc<App> {
        let _ = fs::create_dir_all(&user_data); // Electron creates userData before main.js runs
        let file = user_data.join(store::FILE_NAME);
        let log_file = user_data.join(LOG_NAME);
        let retry = host.packaged().then(|| {
            deps.retry_task.unwrap_or_else(|| {
                let exe = std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                Rc::new(PsRetryTask::new(
                    exe,
                    user_data.display().to_string(),
                    deps.exec.clone(),
                ))
            })
        });
        let retry_arg = argv.iter().find(|a| a.starts_with(RETRY_ARG)).cloned();
        let scheduled_at = retry_arg.as_deref().map_or(0.0, |a| {
            js::string_to_number(a.split('=').nth(1).unwrap_or_default())
        });
        let hidden = argv.iter().any(|a| a == HIDDEN_ARG);
        let loaded = store::load_db(&file);
        let env = deps.env.clone();
        let round = deps.round.unwrap_or_else(|| {
            Rc::new(move |d: Rc<dyn Driver>, o: RoundOpts| {
                let env = env.clone();
                Box::pin(async move { engine::round(&*d, &o, &*env).await })
            })
        });
        let app = Rc::new(App {
            geo: GeoCache::new(deps.clock.clone(), deps.exec.clone()),
            host,
            clock: deps.clock,
            write_json: deps.write_json,
            round,
            retry,
            file,
            log_file,
            unattended: retry_arg.is_some() || hidden,
            retry_arg,
            scheduled_at,
            hidden,
            load_error: loaded.load_error,
            recovered_from: loaded.recovered_from,
            st: RefCell::new(State {
                db: loaded.db,
                status: Status {
                    code: "idle",
                    text: "Iniciando…".into(),
                    next: 0.0,
                },
                logs: VecDeque::new(),
                win: false,
                page: None,
                drv: None,
                stop_once: false,
                timer: None,
                login_poll: None,
                midnight: None,
                busy: false,
                fresh: true,
                offline: false,
                quitting: false,
                attempting: false,
                schedule_version: 0,
                pending: None,
                fails: 0,
            }),
            events: RefCell::new(events),
            quit: Notify::new(),
        });
        app.midnight();
        app
    }

    // ---------- db, log, status ----------

    fn db(&self, key: &str) -> Value {
        self.st.borrow().db.get(key).cloned().unwrap_or(Value::Null)
    }

    fn set_db(&self, key: &str, v: Value) {
        self.st.borrow_mut().db.insert(key.into(), v);
    }

    fn enabled(&self) -> bool {
        truthy(&self.db("enabled"))
    }

    fn quitting(&self) -> bool {
        self.st.borrow().quitting
    }

    /// `save()`: never throws.
    fn save(&self) -> bool {
        let db = Value::Object(self.st.borrow().db.clone());
        match (self.write_json)(&self.file, &db) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("save {e}");
                false
            }
        }
    }

    fn log(&self, msg: &str) {
        let line = format!("{}  {msg}", clock::log_stamp(self.clock.now()));
        {
            let mut st = self.st.borrow_mut();
            st.logs.push_front(line.clone());
            st.logs.truncate(60);
        }
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
        {
            let _ = f.write_all(format!("{line}\n").as_bytes());
        }
        self.push();
    }

    fn today(&self) {
        let d = clock::to_date_string(self.clock.now());
        let mut st = self.st.borrow_mut();
        if !matches!(st.db.get("day"), Some(Value::String(s)) if *s == d) {
            st.db.insert("day".into(), Value::String(d));
            st.db.insert("likesToday".into(), num(0.0));
        }
    }

    /// `midnight()`: at the next local 00:00:01, `today(); save(); push()` and re-arm.
    fn midnight(self: &Rc<Self>) {
        let now = self.clock.now();
        let next = clock::next_midnight(now);
        let me = self.clone();
        let h = spawn_local(async move {
            sleep_ms(next - now).await;
            me.today();
            me.save();
            me.push();
            me.midnight();
        });
        self.st.borrow_mut().midnight = Some(h.abort_handle());
    }

    fn set_status(&self, code: &'static str, text: &str, next: f64) {
        let changed = {
            let mut st = self.st.borrow_mut();
            let changed = text != st.status.text;
            st.status = Status {
                code,
                text: text.into(),
                next,
            };
            changed
        };
        if changed {
            self.log(text)
        } else {
            self.push()
        }
        self.build_tray(true);
    }

    /// `tray?.setToolTip(...)` (when `tooltip`) + `buildTray()`.
    fn build_tray(&self, tooltip: bool) {
        let (win, text) = {
            let st = self.st.borrow();
            (st.win, st.status.text.clone())
        };
        if win {
            let tip = if tooltip {
                format!("Spark — {text}")
            } else {
                String::new()
            };
            self.host.set_tray(&tip, &text, self.enabled());
        }
    }

    fn snapshot(&self) -> Value {
        let st = self.st.borrow();
        let mut m = st.db.clone();
        m.insert(
            "status".into(),
            json!({ "code": st.status.code, "text": st.status.text, "next": num(st.status.next) }),
        );
        m.insert(
            "logs".into(),
            Value::Array(st.logs.iter().cloned().map(Value::String).collect()),
        );
        Value::Object(m)
    }

    fn push(&self) {
        self.today();
        let send = {
            let st = self.st.borrow();
            !st.quitting && st.win
        };
        if send {
            self.host.send_state(&self.snapshot());
        }
    }

    // ---------- window ----------

    /// `win && !win.isMinimized() && win.isVisible() && win.getPosition()[0] > -10000`.
    fn visible_on_screen(&self) -> bool {
        if !self.st.borrow().win {
            return false;
        }
        let w = self.host.window();
        !w.minimized && w.visible && w.x > -10000
    }

    fn minimize_if_visible(&self) {
        if truthy(&self.db("startMinimized")) && self.visible_on_screen() {
            self.host.minimize();
        }
    }

    // ---------- autostart ----------

    fn sync_autostart(&self) {
        if !self.host.packaged() {
            return;
        }
        let login = self.host.login_item();
        let wanted = self.db("autostart");
        let Ok(plan) = engine::autostart_plan(&login, &wanted) else {
            return;
        };
        if truthy(&plan["register"]) {
            self.host.set_login_item(true, None);
        }
        // without a Windows entry plan.autostart IS db.autostart (same value, `!==` is false)
        if login.open_at_login && !js::strict_eq(&wanted, &plan["autostart"]) {
            self.set_db("autostart", plan["autostart"].clone());
            self.save();
        }
    }

    // ---------- waits ----------

    fn clear_timer(&self) {
        if let Some(t) = self.st.borrow_mut().timer.take() {
            t.abort();
        }
    }

    fn stop_wait(&self) {
        self.clear_timer();
        self.st.borrow_mut().pending = None;
    }

    fn clear_login_poll(&self) {
        if let Some(t) = self.st.borrow_mut().login_poll.take() {
            t.abort();
        }
    }

    fn wait(self: &Rc<Self>, min: f64, why: &str, code: &'static str) {
        if !self.enabled() {
            return self.set_status("paused", "Pausado", 0.0);
        }
        self.st.borrow_mut().pending = Some(Pending {
            start: self.clock.now(),
            min,
            why: why.into(),
            code,
        });
        self.arm(0.0);
    }

    fn offline_wait(self: &Rc<Self>) {
        let i = {
            let mut st = self.st.borrow_mut();
            let i = st.fails.min(OFFLINE_BACKOFF_S.len() - 1);
            st.fails += 1;
            i
        };
        self.wait(
            OFFLINE_BACKOFF_S[i] / 60.0,
            "Sem conexão com o Tinder",
            "error",
        );
    }

    /// `arm(soonest)`: deadline from the wait's start; an expired one runs now (or in `soonest` ms).
    fn arm(self: &Rc<Self>, soonest: f64) {
        self.clear_timer();
        let Some(p) = self.st.borrow().pending.clone() else {
            return;
        };
        let now = self.clock.now();
        let next = f64::max(p.start + p.min * MIN, now + soonest);
        let me = self.clone();
        let h = spawn_local(async move {
            sleep_ms(f64::min(next - now, TIMEOUT_MAX)).await;
            me.wait_fired(next);
        });
        self.st.borrow_mut().timer = Some(h.abort_handle());
        let at = clock::time_hm(next);
        self.set_status(p.code, &format!("{} · testo de novo às {at}", p.why), next);
    }

    fn wait_fired(self: &Rc<Self>, next: f64) {
        if self.clock.now() < next {
            return self.arm(0.0);
        }
        self.minimize_if_visible();
        self.cycle(false);
    }

    /// `waitLikes(close)`: its synchronous part runs now; the returned future is the rest of the promise.
    fn wait_likes(self: &Rc<Self>, close: bool) -> Fut<'static, ()> {
        let (start, min) = (
            js::to_number(&self.db("likesWaitFrom")),
            js::to_number(&self.db("retryMin")),
        );
        self.st.borrow_mut().pending = Some(Pending {
            start,
            min,
            why: "Likes esgotados".into(),
            code: "waiting",
        });
        self.arm(0.0);
        let Some(task) = self.retry.clone() else {
            return Box::pin(async {});
        };
        let version = {
            let mut st = self.st.borrow_mut();
            st.schedule_version += 1;
            st.schedule_version
        };
        let next = start + min * MIN;
        let scheduled = (|| -> Result<Fut<'static, Result<Value, String>>, String> {
            self.save();
            let saved = read_saved(&self.file)?;
            for k in ["enabled", "retryMin", "likesWaitFrom"] {
                if !js::strict_eq_opt(prop(&saved, k)?, Some(&self.db(k))) {
                    return Err("A espera não foi salva corretamente".into());
                }
            }
            Ok(task.schedule(next))
        })();
        let me = self.clone();
        match scheduled {
            Err(e) => {
                me.schedule_failed(version, &e);
                me.paused_unless_quitting();
                Box::pin(async {})
            }
            Ok(f) => Box::pin(async move {
                match f.await {
                    Err(e) => me.schedule_failed(version, &e),
                    Ok(_) => {
                        let current = me.st.borrow().schedule_version == version;
                        if current && me.enabled() && !me.quitting() {
                            me.log("Próxima abertura minimizada agendada no Windows; pause o Automático para cancelar");
                            if close {
                                me.host.notify(&format!(
                                    "Curtidas esgotadas. Próxima verificação às {}.",
                                    clock::time_hm(next)
                                ));
                                me.quit_app().await;
                            }
                        }
                    }
                }
                me.paused_unless_quitting();
            }),
        }
    }

    fn schedule_failed(&self, version: u64, e: &str) {
        let (current, next) = {
            let st = self.st.borrow();
            (
                st.schedule_version == version && !st.quitting,
                st.status.next,
            )
        };
        if current {
            self.set_status(
                "error",
                &format!("Não consegui agendar a reabertura; continuarei aberto: {e}"),
                next,
            );
        }
    }

    fn paused_unless_quitting(&self) {
        if !self.enabled() && !self.quitting() {
            self.set_status("paused", "Pausado", 0.0);
        }
    }

    fn cancel_retry(self: &Rc<Self>) {
        self.st.borrow_mut().schedule_version += 1;
        if let Some(task) = &self.retry {
            let f = task.cancel();
            let me = self.clone();
            spawn_local(async move {
                if let Err(e) = f.await {
                    me.log(&format!("Não consegui remover o agendamento: {e}"));
                }
            });
        }
    }

    fn finish_wait(self: &Rc<Self>) {
        let previous = self.db("likesWaitFrom");
        if !truthy(&previous) {
            return;
        }
        self.set_db("likesWaitFrom", num(0.0));
        if self.save() {
            self.stop_wait();
            self.cancel_retry();
        } else {
            self.set_db("likesWaitFrom", previous);
        }
    }

    /// `quitApp()`: once; the returned future ends with `app.quit()`.
    fn quit_app(self: &Rc<Self>) -> Fut<'static, ()> {
        if self.quitting() {
            return Box::pin(async {});
        }
        self.st.borrow_mut().quitting = true;
        self.stop_wait();
        self.clear_login_poll();
        let idle = self.retry.as_ref().map(|t| t.idle());
        let me = self.clone();
        Box::pin(async move {
            match idle {
                Some(i) => i.await,
                None => tokio::task::yield_now().await,
            }
            me.app_quit();
        })
    }

    fn app_quit(&self) {
        self.host.quit();
        self.quit.notify_one();
    }

    fn wait_login(self: &Rc<Self>, text: Option<&str>) {
        let text = text.unwrap_or("Faça login no Tinder para começar");
        self.set_status("login", text, 0.0);
        if !self.unattended && !truthy(&self.db("startMinimized")) {
            self.host.from_tray();
        }
        self.host.notify(&format!("{text}."));
        self.clear_login_poll();
        // only watches, never reloads or touches: does not disturb someone typing the login
        let checking = Rc::new(std::cell::Cell::new(false));
        let me = self.clone();
        let h = spawn_local(async move {
            loop {
                sleep_ms(5000.0).await;
                if checking.get() {
                    continue;
                }
                checking.set(true);
                let (me, checking) = (me.clone(), checking.clone());
                spawn_local(async move {
                    let drv = me.st.borrow().drv.clone();
                    if let Some(d) = drv
                        && !d.gone()
                        && d.logged_in().await == Ok(true)
                    {
                        me.clear_login_poll();
                        me.cycle(false);
                    }
                    checking.set(false);
                });
            }
        });
        self.st.borrow_mut().login_poll = Some(h.abort_handle());
    }

    // ---------- cycle ----------

    /// `cycle(force)`: the checks and the start run now; the rounds run in a spawned task.
    fn cycle(self: &Rc<Self>, force: bool) {
        {
            let st = self.st.borrow();
            if st.quitting || st.busy || st.drv.as_ref().is_none_or(|d| d.gone()) {
                return;
            }
        }
        if !self.enabled() {
            return self.set_status("paused", "Pausado", 0.0);
        }
        if !force {
            self.minimize_if_visible();
        }
        let waiting = self.db("likesWaitFrom");
        let attempting = self.st.borrow().attempting;
        if !force
            && !attempting
            && truthy(&waiting)
            && self.clock.now()
                < js::to_number(&waiting) + js::to_number(&self.db("retryMin")) * MIN
        {
            spawn_local(self.wait_likes(self.unattended));
            return;
        }
        self.stop_wait();
        // the task stays valid if the PC shuts down during an attempt not yet concluded
        if truthy(&waiting) {
            self.st.borrow_mut().schedule_version += 1;
        } else {
            self.cancel_retry();
        }
        {
            let mut st = self.st.borrow_mut();
            st.busy = true;
            st.attempting = true;
        }
        self.set_status("running", "Procurando perfis…", 0.0);
        let me = self.clone();
        spawn_local(async move {
            if let Err(e) = me.rounds(force).await {
                me.wait(1.0, &format!("Erro: {}", first_line(&e)), "error");
            }
            me.st.borrow_mut().busy = false;
        });
    }

    fn round_opts(self: &Rc<Self>) -> RoundOpts {
        let skip_first = truthy(&self.db("skipFirst"));
        let (a, b, c) = (self.clone(), self.clone(), self.clone());
        RoundOpts {
            skip_first,
            per_load: Some(if skip_first { 2 } else { 1 }),
            delay: [1200.0, 2800.0],
            // without network the card never gets ready: stop now instead of waiting 45 s
            stop: Box::new(move || a.quitting() || !a.enabled() || a.st.borrow().offline),
            on_like: Box::new(move |d| {
                b.today();
                let (today, total) = (
                    js::max0(&js::plus(&b.db("likesToday"), d as f64)),
                    js::max0(&js::plus(&b.db("likesTotal"), d as f64)),
                );
                b.set_db("likesToday", num(today));
                b.set_db("likesTotal", num(total));
                b.save();
                b.push();
            }),
            on_match: Box::new(move || {
                c.set_db("matches", num(js::to_number(&c.db("matches")) + 1.0));
                c.save();
                c.log("Novo match! 🔥");
                c.host.notify("Você tem um novo match! 🔥");
            }),
        }
    }

    /// The `try` block of cycle(): first card left, second right, reload; until the likes run out.
    async fn rounds(self: &Rc<Self>, force: bool) -> Result<(), String> {
        let mut r: Option<RoundResult> = None;
        let (mut likes, mut retry) = (0i64, true);
        loop {
            let drv = self
                .st
                .borrow()
                .drv
                .clone()
                .ok_or("Cannot read properties of undefined (reading 'reload')")?;
            let fresh = std::mem::replace(&mut self.st.borrow_mut().fresh, false);
            if !fresh {
                drv.reload().await;
                self.pump();
            }
            // the page did not load (no internet, common right after logging into Windows): back off
            if self.st.borrow().offline {
                self.offline_wait();
                return Ok(());
            }
            if !drv.logged_in().await? {
                self.wait_login(None);
                return Ok(());
            }
            if r.is_none() {
                self.set_status("running", "Dando likes", 0.0);
            }
            let rr = (self.round)(drv, self.round_opts()).await?;
            self.pump();
            likes += rr.likes;
            if matches!(rr.reason, Reason::Reload | Reason::Empty) {
                self.finish_wait();
            }
            if !self.st.borrow().offline {
                self.st.borrow_mut().fails = 0;
            }
            let mut again = rr.reason == Reason::Reload;
            if again {
                retry = true;
            } else if rr.reason == Reason::Stuck && retry {
                retry = false;
                again = true;
                self.log("Tela travada; recarrego e tento de novo");
            }
            self.save();
            self.push();
            r = Some(rr);
            if !again {
                break;
            }
        }
        if likes != 0 {
            self.log(&format!("{likes} likes nesta rodada"));
        }
        let reason = r.map_or(Reason::Stuck, |r| r.reason);
        match reason {
            Reason::Login => self.wait_login(None),
            Reason::Stopped => {
                if self.st.borrow().offline {
                    self.offline_wait()
                } else if self.enabled() {
                    let me = self.clone();
                    spawn_local(async move { me.cycle(false) }); // setImmediate(cycle)
                } else {
                    self.set_status("paused", "Pausado", 0.0)
                }
            }
            Reason::Out => {
                self.st.borrow_mut().attempting = false;
                self.set_db("likesWaitFrom", num(self.clock.now()));
                self.save();
                // a manual check or a window on screen stays open to show the status and the timer
                let on_screen = self.st.borrow().win && {
                    let w = self.host.window();
                    !w.minimized && w.x > -10000
                };
                let close = !force && !on_screen;
                if self.enabled() {
                    self.wait_likes(close).await
                } else {
                    self.set_status("paused", "Pausado", 0.0)
                }
            }
            Reason::Empty => {
                self.st.borrow_mut().attempting = false;
                if self.enabled() {
                    self.set_status("waiting", "Sem perfis por perto", 0.0)
                } else {
                    self.set_status("paused", "Pausado", 0.0)
                }
            }
            Reason::Reload | Reason::Stuck => self.wait(5.0, "Tela inesperada", "waiting"),
        }
        Ok(())
    }

    // ---------- IPC and events ----------

    fn toggle(self: &Rc<Self>, on: Value) {
        let on_truthy = truthy(&on);
        self.set_db("enabled", on);
        self.save();
        if on_truthy {
            let (busy, attempting) = {
                let st = self.st.borrow();
                (st.busy, st.attempting)
            };
            if busy && truthy(&self.db("likesWaitFrom")) && !attempting {
                spawn_local(self.wait_likes(false));
            } else {
                self.cycle(false);
            }
        } else {
            self.st.borrow_mut().attempting = false;
            self.stop_wait();
            self.cancel_retry();
            if !self.st.borrow().busy {
                self.set_status("paused", "Pausado", 0.0);
            }
        }
        self.push();
    }

    fn set(self: &Rc<Self>, k: Option<&Value>, v: Option<&Value>) {
        let Some(Value::String(k)) = k else { return };
        if !SETTABLE.contains(&k.as_str()) {
            return;
        }
        if js::type_of(v) != js::type_of(self.st.borrow().db.get(k)) {
            return; // only values of the right type
        }
        if k == "retryMin"
            && !v
                .and_then(js::number)
                .is_some_and(|f| RETRY_MINUTES.contains(&f))
        {
            return;
        }
        let Some(v) = v.cloned() else { return };
        let on = truthy(&v);
        self.set_db(k, v);
        self.save();
        if k == "autostart" && self.host.packaged() {
            self.host.set_login_item(on, Some(on));
        }
        if k == "retryMin"
            && self.enabled()
            && truthy(&self.db("likesWaitFrom"))
            && !self.st.borrow().attempting
        {
            spawn_local(self.wait_likes(false));
        }
        self.push();
    }

    fn second_instance(self: &Rc<Self>, argv: &[String]) {
        if let Some(retry) = argv.iter().find(|a| a.starts_with(RETRY_ARG)) {
            let at = js::string_to_number(retry.split('=').nth(1).unwrap_or_default());
            if js::is_safe_integer(at)
                && at > 0.0
                && self.enabled()
                && truthy(&self.db("likesWaitFrom"))
            {
                self.minimize_if_visible();
                self.cycle(false);
            }
        } else if self.st.borrow().win {
            self.host.from_tray();
        }
    }

    fn attached(self: &Rc<Self>) {
        let Some(page) = self.host.page() else { return };
        let mut st = self.st.borrow_mut();
        st.drv = Some(Rc::new(WebDriver::new(page.clone(), HOME)));
        st.page = Some(page);
        st.stop_once = true;
    }

    fn render_gone(self: &Rc<Self>, reason: &str) {
        self.log(&format!(
            "A página do Tinder caiu ({reason}); recarrego no próximo ciclo"
        ));
        let Some(page) = self.st.borrow().page.clone() else {
            return;
        };
        if !page.destroyed() {
            let me = self.clone();
            spawn_local(async move {
                sleep_ms(2000.0).await;
                if !me.quitting() && !page.destroyed() {
                    let _ = page.load(HOME).await;
                }
            });
        }
    }

    fn start_loading(self: &Rc<Self>) {
        self.st.borrow_mut().offline = false;
        let (f, host) = (self.geo.get(), self.host.clone());
        spawn_local(async move { host.set_geolocation(f.await.as_ref().map(geo::coords)) });
    }

    fn handle(self: &Rc<Self>, ev: HostEvent) {
        match ev {
            HostEvent::Ipc { channel, args } => match channel.as_str() {
                "toggle" => self.toggle(args.first().cloned().unwrap_or(Value::Null)),
                "check" => self.cycle(true),
                "win" => match args.first().and_then(Value::as_str) {
                    Some("close" | "quit") => {
                        spawn_local(self.quit_app());
                    }
                    Some("min") => self.host.minimize(),
                    _ => self.host.toggle_maximize(),
                },
                "set" => self.set(args.first(), args.get(1)),
                _ => {}
            },
            HostEvent::InvokeState { id } => {
                self.today();
                self.host.reply_state(id, &self.snapshot());
            }
            HostEvent::Attached => self.attached(),
            HostEvent::StartLoading => self.start_loading(),
            HostEvent::StopLoading => {
                let first = std::mem::take(&mut self.st.borrow_mut().stop_once);
                if first {
                    self.cycle(false);
                }
            }
            HostEvent::NetError(e) => {
                if spark_main_pure::port::net_down_str(&e) {
                    self.st.borrow_mut().offline = true;
                }
            }
            HostEvent::FailLoad => self.st.borrow_mut().offline = true,
            HostEvent::RenderGone(reason) => self.render_gone(&reason),
            HostEvent::SecondInstance(argv) => self.second_instance(&argv),
            HostEvent::Resume => {
                if self.st.borrow().pending.is_some() {
                    self.arm(15_000.0);
                }
            }
            HostEvent::SessionEnd => {
                if self.load_error.is_none() {
                    self.save();
                }
            }
            HostEvent::TrayClick => self.host.from_tray(),
            HostEvent::TrayToggle => self.toggle(Value::Bool(!self.enabled())),
            HostEvent::TrayCheck => self.cycle(true),
            HostEvent::TrayQuit | HostEvent::WindowClose => {
                spawn_local(self.quit_app());
            }
        }
    }

    /// Handles the events already delivered. Called after awaiting the page, so a FailLoad / NetError sent
    /// before the host resolved the call is seen as main.js sees it (its handlers ran first).
    fn pump(self: &Rc<Self>) {
        loop {
            let ev = self.events.borrow_mut().try_recv();
            match ev {
                Ok(ev) => self.handle(ev),
                Err(_) => break,
            }
        }
    }

    async fn event_loop(self: &Rc<Self>) {
        loop {
            let ev = tokio::select! {
                ev = std::future::poll_fn(|cx| self.events.borrow_mut().poll_recv(cx)) => ev,
                () = self.quit.notified() => None,
            };
            match ev {
                Some(ev) => self.handle(ev),
                None => break,
            }
        }
    }

    /// The process is ending: nothing more runs (Node would exit here).
    fn shutdown(&self) {
        self.st.borrow_mut().quitting = true;
        self.stop_wait();
        self.clear_login_poll();
        if let Some(t) = self.st.borrow_mut().midnight.take() {
            t.abort();
        }
    }

    // ---------- startup ----------

    fn create_window(&self) {
        let placement = if self.retry_arg.is_some() || truthy(&self.db("startMinimized")) {
            Placement::Minimized
        } else if self.hidden {
            Placement::Tray
        } else {
            Placement::Shown
        };
        self.st.borrow_mut().win = true;
        self.host.create_window(placement);
        self.build_tray(false); // the tray's first buildTray(): no tooltip yet
    }

    /// `app.whenReady().then(...)`; false = quit without a window.
    async fn start(self: &Rc<Self>) -> bool {
        let (enabled, waiting) = (self.enabled(), truthy(&self.db("likesWaitFrom")));
        // an interrupted task may carry an old argument; the saved deadline rules
        if self.retry_arg.is_some()
            && (!js::is_safe_integer(self.scheduled_at)
                || self.scheduled_at <= 0.0
                || !enabled
                || !waiting)
        {
            if !enabled || !waiting {
                self.cancel_retry();
                if let Some(t) = &self.retry {
                    t.idle().await;
                }
            }
            self.app_quit();
            return false;
        }
        if !enabled || !waiting {
            self.cancel_retry();
        }
        if fs::metadata(&self.log_file).is_ok_and(|m| m.len() > 1_000_000) {
            let mut old = self.log_file.clone().into_os_string();
            old.push(".old");
            let _ = fs::rename(&self.log_file, old);
        }
        if self.load_error.is_some() {
            self.log("Configurações inválidas; Automático pausado. Cópia em spark.json.corrompido");
        }
        if let Some(from) = &self.recovered_from {
            let paused = if from.ends_with(".bak") {
                "; Automático pausado para preservar sua última escolha"
            } else {
                ""
            };
            self.log(&format!("Configurações recuperadas de {from}{paused}"));
        }
        drop(self.geo.get()); // windowsGeo(): the position is ready for the first Tinder page
        self.sync_autostart();
        self.create_window();
        true
    }
}
