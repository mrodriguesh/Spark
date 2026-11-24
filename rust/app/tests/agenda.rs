//! test/agenda.js ported: main.js's scheduling for real (fake host, fake page, fake retry task,
//! controlled clock, no Tinder). Each original assertion message is kept as a comment next to its assert.
//! Differences from the JS harness: events go through the core's channel, so the test flushes before asserting
//! what JS asserted synchronously after an emit; each `boot` runs in its own LocalSet, dropped at the next boot
//! (= `mock.timers.reset()`); SPEC S-05.12: where agenda.js expects the automatic close after "out", the fake
//! window is minimized (`hidden`), since the port implements the real-Electron visibility check.
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    fs,
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicU32, Ordering},
    time::Duration,
};

use serde_json::{Map, Value, json};
use spark::{
    core::{
        Deps,
        clock::{self, Clock, MockClock},
        engine::{Driver, RealEnv, Reason, RoundOpts, RoundResult},
        exec::{Exec, ExecOutput},
        js::number,
        retry::RetryTask,
        run_app,
    },
    iface::{Fut, Host, HostEvent, LoginItem, Page, Placement, WinState},
};
use spark_store::port::{parse, stringify, write_json};
use tokio::{
    sync::{mpsc, oneshot},
    task::LocalSet,
};

const MIN: f64 = 60_000.0;
const H24: f64 = 24.0 * 60.0 * MIN;

/// `new Date('2026-10-19T12:00:00').getTime()` (local time).
fn t0() -> f64 {
    clock::local_ms(2026, 10, 19, 12, 0, 0).unwrap()
}

fn rr(reason: Reason, likes: i64) -> RoundResult {
    RoundResult { reason, likes }
}

type RoundFut = Fut<'static, Result<RoundResult, String>>;
/// One entry of `app.results`: a result, or a function of the instance (`r()` in agenda.js).
enum FakeRound {
    Ready(RoundResult),
    Run(Box<dyn FnOnce(&Shared) -> RoundFut>),
}
use FakeRound::{Ready, Run};

type ScheduleFn = Rc<dyn Fn(&Shared, f64) -> Fut<'static, Result<Value, String>>>;

/// `app` of agenda.js: the state of the simulated instance.
struct Shared {
    dir: PathBuf,
    tx: mpsc::UnboundedSender<HostEvent>,
    ui: RefCell<Value>,
    calls: Cell<u32>,
    quits: Cell<u32>,
    cancelled: Cell<u32>,
    scheduled: RefCell<Vec<f64>>,
    results: RefCell<VecDeque<FakeRound>>,
    minimized: Cell<bool>,
    win: Cell<Option<WinState>>,
    hidden: bool,
    packaged: bool,
    attached: Cell<bool>,
    page: Rc<FakePage>,
    schedule: Option<ScheduleFn>,
}

/// The `<webview>` webContents: `loadURL` emits did-start-loading, or did-fail-load when `fail`.
struct FakePage {
    url: RefCell<String>,
    loads: Cell<u32>,
    fail: Cell<bool>,
    tx: mpsc::UnboundedSender<HostEvent>,
}

impl Page for FakePage {
    fn eval(&self, _script: &str) -> Fut<'_, Result<Value, String>> {
        Box::pin(async { Ok(Value::Null) })
    }
    fn key(&self, _key_code: &'static str) -> Fut<'_, ()> {
        Box::pin(async {})
    }
    fn load(&self, _url: &str) -> Fut<'_, Result<(), String>> {
        self.loads.set(self.loads.get() + 1);
        let _ = self.tx.send(if self.fail.get() {
            HostEvent::FailLoad
        } else {
            HostEvent::StartLoading
        });
        Box::pin(async { Ok(()) })
    }
    fn url(&self) -> String {
        self.url.borrow().clone()
    }
    fn destroyed(&self) -> bool {
        false
    }
}

struct FakeHost(Rc<Shared>);

impl Host for FakeHost {
    fn create_window(&self, placement: Placement) {
        let mut w = WinState {
            minimized: false,
            visible: true,
            x: 0,
        };
        match placement {
            Placement::Minimized => {
                w.minimized = true;
                self.0.minimized.set(true);
            }
            Placement::Tray => w.x = -32000,
            Placement::Shown => {}
        }
        w.minimized |= self.0.hidden;
        self.0.win.set(Some(w));
    }
    fn send_state(&self, snapshot: &Value) {
        *self.0.ui.borrow_mut() = snapshot.clone();
    }
    fn reply_state(&self, _id: u64, _snapshot: &Value) {}
    fn set_tray(&self, _tooltip: &str, _status_text: &str, _enabled: bool) {}
    fn notify(&self, _body: &str) {} // Notification.isSupported() === false in agenda.js
    fn window(&self) -> WinState {
        self.0.win.get().unwrap_or_default()
    }
    fn minimize(&self) {
        self.0.minimized.set(true);
        if let Some(mut w) = self.0.win.get() {
            w.minimized = true;
            self.0.win.set(Some(w));
        }
    }
    fn toggle_maximize(&self) {}
    fn from_tray(&self) {
        if let Some(mut w) = self.0.win.get() {
            w.minimized = false;
            w.visible = true;
            if w.x <= -10000 {
                w.x = 0;
            }
            self.0.win.set(Some(w));
        }
    }
    fn quit(&self) {
        self.0.quits.set(self.0.quits.get() + 1);
    }
    fn page(&self) -> Option<Rc<dyn Page>> {
        self.0
            .attached
            .get()
            .then(|| self.0.page.clone() as Rc<dyn Page>)
    }
    fn packaged(&self) -> bool {
        self.0.packaged
    }
    fn login_item(&self) -> LoginItem {
        LoginItem::default() // getLoginItemSettings: () => ({})
    }
    fn set_login_item(&self, _open_at_login: bool, _enabled: Option<bool>) {}
    fn set_geolocation(&self, _geo: Option<(f64, f64, f64)>) {}
}

/// `app.retryTask` of agenda.js.
struct FakeRetry(Rc<Shared>);

impl RetryTask for FakeRetry {
    fn schedule(&self, at: f64) -> Fut<'static, Result<Value, String>> {
        self.0.scheduled.borrow_mut().push(at);
        match &self.0.schedule {
            Some(f) => f(&self.0, at),
            None => Box::pin(async { Ok(Value::Null) }),
        }
    }
    fn cancel(&self) -> Fut<'static, Result<Value, String>> {
        self.0.cancelled.set(self.0.cancelled.get() + 1);
        Box::pin(async { Ok(Value::Null) })
    }
    fn idle(&self) -> Fut<'static, ()> {
        Box::pin(async {})
    }
}

/// `child_process.execFile` mock: no Windows position.
struct NoGeo;
impl Exec for NoGeo {
    fn exec(&self, _file: &str, _args: &[String], _timeout_ms: u64) -> Fut<'static, ExecOutput> {
        Box::pin(async { ExecOutput::default() })
    }
}

#[derive(Default)]
struct Platform {
    packaged: bool,
    hidden: bool,
    args: Vec<String>,
    raw: Option<String>,
    backup: Option<String>,
    temporary: Option<String>,
    schedule: Option<ScheduleFn>,
    write_fails: bool,
}

fn packaged() -> Platform {
    Platform {
        packaged: true,
        ..Default::default()
    }
}

fn retry_at(at: f64) -> Vec<String> {
    vec![format!("--spark-retry-at={at}")]
}

/// `deferred()`: (done, p).
fn deferred<T: 'static>() -> (Box<dyn FnOnce(T)>, Fut<'static, T>) {
    let (tx, rx) = oneshot::channel();
    (
        Box::new(move |v| {
            let _ = tx.send(v);
        }),
        Box::pin(async move {
            match rx.await {
                Ok(v) => v,
                Err(_) => std::future::pending().await,
            }
        }),
    )
}

fn later(p: Fut<'static, RoundResult>) -> FakeRound {
    Run(Box::new(move |_| Box::pin(async move { Ok(p.await) })))
}

struct Boot {
    s: Rc<Shared>,
    clock: Rc<MockClock>,
    local: LocalSet,
}

static DIRS: AtomicU32 = AtomicU32::new(0);

/// `boot(cfg, results, platform)`: a new main.js instance over the given spark.json.
async fn boot(cfg: Value, results: Vec<FakeRound>, p: Platform) -> Boot {
    let dir = std::env::temp_dir().join(format!(
        "spark-agenda-{}-{}",
        std::process::id(),
        DIRS.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let mut m = Map::new();
    m.insert("day".into(), Value::String(clock::to_date_string(t0())));
    m.insert("autostart".into(), Value::Bool(false));
    for (k, v) in cfg.as_object().cloned().unwrap_or_default() {
        m.insert(k, v);
    }
    fs::write(dir.join("spark.json"), stringify(&Value::Object(m))).unwrap();
    for (text, suffix) in [(&p.raw, ""), (&p.backup, ".bak"), (&p.temporary, ".tmp")] {
        if let Some(t) = text {
            fs::write(dir.join(format!("spark.json{suffix}")), t).unwrap();
        }
    }
    let (tx, rx) = mpsc::unbounded_channel();
    let page = Rc::new(FakePage {
        url: RefCell::new("https://tinder.com/app/recs".into()),
        loads: Cell::new(0),
        fail: Cell::new(false),
        tx: tx.clone(),
    });
    let s = Rc::new(Shared {
        dir: dir.clone(),
        tx: tx.clone(),
        ui: RefCell::new(Value::Null),
        calls: Cell::new(0),
        quits: Cell::new(0),
        cancelled: Cell::new(0),
        scheduled: RefCell::new(vec![]),
        results: RefCell::new(results.into()),
        minimized: Cell::new(false),
        win: Cell::new(None),
        hidden: p.hidden,
        packaged: p.packaged,
        attached: Cell::new(false),
        page,
        schedule: p.schedule,
    });
    let clock = Rc::new(MockClock::new(t0()));
    let s2 = s.clone();
    let round = Rc::new(move |_d: Rc<dyn Driver>, _o: RoundOpts| -> RoundFut {
        s2.calls.set(s2.calls.get() + 1);
        let next = s2.results.borrow_mut().pop_front();
        match next {
            None => Box::pin(async { Ok(rr(Reason::Out, 0)) }),
            Some(Ready(r)) => Box::pin(async move { Ok(r) }),
            Some(Run(f)) => f(&s2),
        }
    });
    let deps = Deps {
        clock: clock.clone(),
        exec: Rc::new(NoGeo),
        retry_task: Some(Rc::new(FakeRetry(s.clone()))),
        write_json: if p.write_fails {
            Rc::new(|_: &std::path::Path, _: &Value| Err("falha de gravação simulada".into()))
        } else {
            Rc::new(|f: &std::path::Path, v: &Value| write_json(f, v))
        },
        env: Rc::new(RealEnv::new()),
        round: Some(round),
    };
    let argv = std::iter::once("Spark.exe".to_string())
        .chain(p.args)
        .collect();
    let local = LocalSet::new();
    local.spawn_local(run_app(deps, argv, dir, Rc::new(FakeHost(s.clone())), rx));
    let b = Boot { s, clock, local };
    b.flush().await;
    if b.s.win.get().is_none() {
        return b;
    }
    b.s.attached.set(true);
    b.emit(HostEvent::Attached);
    b.emit(HostEvent::StopLoading); // the page loaded: starts (or resumes the wait)
    b.flush().await;
    b
}

impl Boot {
    async fn flush(&self) {
        self.local
            .run_until(async {
                for _ in 0..100 {
                    tokio::task::yield_now().await;
                }
            })
            .await;
    }
    async fn tick(&self, ms: f64) {
        self.local
            .run_until(tokio::time::advance(Duration::from_nanos(
                (ms * 1e6) as u64,
            )))
            .await;
        self.flush().await;
    }
    fn emit(&self, ev: HostEvent) {
        let _ = self.s.tx.send(ev);
    }
    fn ipc(&self, channel: &str, args: Vec<Value>) {
        self.emit(HostEvent::Ipc {
            channel: channel.into(),
            args,
        });
    }
    fn check(&self) {
        self.ipc("check", vec![]);
    }
    fn toggle(&self, on: bool) {
        self.ipc("toggle", vec![Value::Bool(on)]);
    }
    fn resume(&self) {
        self.emit(HostEvent::Resume);
    }
    async fn set(&self, k: &str, v: Value) {
        self.ipc("set", vec![Value::from(k), v]);
        self.flush().await;
    }
    fn ui(&self) -> Value {
        self.s.ui.borrow().clone()
    }
    fn code(&self) -> String {
        self.ui()["status"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
    fn text(&self) -> String {
        self.ui()["status"]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }
    fn why(&self) -> String {
        self.text()
            .split(" · ")
            .next()
            .unwrap_or_default()
            .to_string()
    }
    fn next(&self) -> f64 {
        number(&self.ui()["status"]["next"]).unwrap_or(f64::NAN)
    }
    fn now(&self) -> f64 {
        self.clock.now()
    }
    fn cfg(&self) -> Value {
        parse(&fs::read_to_string(self.s.dir.join("spark.json")).unwrap()).unwrap()
    }
    fn cfg_num(&self, k: &str) -> f64 {
        number(&self.cfg()[k]).unwrap_or(f64::NAN)
    }
    fn calls(&self) -> u32 {
        self.s.calls.get()
    }
    fn quits(&self) -> u32 {
        self.s.quits.get()
    }
    fn results(&self, r: Vec<FakeRound>) {
        *self.s.results.borrow_mut() = r.into();
    }
    fn push(&self, r: FakeRound) {
        self.s.results.borrow_mut().push_back(r);
    }
    fn logs(&self) -> Vec<String> {
        self.ui()["logs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|l| l.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for Boot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.s.dir);
    }
}

fn out() -> FakeRound {
    Ready(rr(Reason::Out, 0))
}
fn empty() -> FakeRound {
    Ready(rr(Reason::Empty, 0))
}

#[tokio::test(start_paused = true)]
async fn every_interval_one_attempt_on_time() {
    let t0 = t0();
    // Cada intervalo dispara exatamente uma tentativa; limite persistente inicia outra espera completa.
    for min in [15.0, 30.0, 60.0, 120.0, 360.0] {
        let app = boot(
            json!({ "retryMin": min }),
            vec![out(), out(), empty()],
            Platform::default(),
        )
        .await;
        assert_eq!(app.calls(), 1);
        assert_eq!(app.why(), "Likes esgotados");
        assert_eq!(app.next(), t0 + min * MIN);
        assert_eq!(app.cfg_num("likesWaitFrom"), t0);
        app.tick(min * MIN - 1.0).await;
        assert_eq!(app.calls(), 1, "não tenta antes da hora");
        app.tick(1.0).await;
        assert_eq!(app.calls(), 2);
        assert_eq!(app.next(), t0 + 2.0 * min * MIN);
        app.tick(min * MIN).await;
        assert_eq!(app.calls(), 3);
        assert_eq!(
            app.next(),
            0.0,
            "sem perfis mantém o comportamento anterior"
        );
        assert_eq!(app.cfg_num("likesWaitFrom"), 0.0);
        app.resume();
        app.tick(H24).await;
        assert_eq!(app.calls(), 3);
    }
    // ok('15/30/60/120/360 min: espera inteira, uma tentativa no prazo, novo limite renova a espera; sem perfis encerra')
}

#[tokio::test(start_paused = true)]
async fn saved_wait_deadline_interval_change_and_validation() {
    let t0 = t0();
    let app = boot(
        json!({ "retryMin": 30, "likesWaitFrom": t0 - 10.0 * MIN }),
        vec![],
        Platform::default(),
    )
    .await;
    assert_eq!(app.calls(), 0, "reiniciar respeita a espera salva");
    assert_eq!(app.next(), t0 + 20.0 * MIN);
    app.set("retryMin", json!(60)).await;
    assert_eq!(
        app.next(),
        t0 + 50.0 * MIN,
        "alteração parte da detecção original"
    );
    app.set("retryMin", json!(15)).await;
    assert_eq!(app.next(), t0 + 5.0 * MIN);
    // NaN and Infinity have no JSON form: over the JSON bridge they arrive as null
    for v in [
        json!(0),
        json!(-1),
        json!(16),
        json!(1440),
        Value::Null,
        Value::Null,
        json!("30"),
        Value::Null,
        json!(true),
    ] {
        app.set("retryMin", v).await;
    }
    assert_eq!(
        app.cfg_num("retryMin"),
        15.0,
        "IPC recusa valores inválidos"
    );
    assert_eq!(app.next(), t0 + 5.0 * MIN);
    app.tick(5.0 * MIN).await;
    assert_eq!(app.calls(), 1);
    let app = boot(
        json!({ "retryMin": 360, "likesWaitFrom": t0 - 40.0 * MIN }),
        vec![],
        Platform::default(),
    )
    .await;
    app.set("retryMin", json!(15)).await;
    app.tick(0.0).await;
    assert_eq!(
        app.calls(),
        1,
        "intervalo encurtado já vencido = uma tentativa"
    );
    let app = boot(
        json!({ "retryMin": 30, "likesWaitFrom": t0 - 40.0 * MIN }),
        vec![],
        Platform::default(),
    )
    .await;
    assert_eq!(
        app.calls(),
        1,
        "reiniciar com prazo vencido = uma tentativa"
    );
    let app = boot(
        json!({ "retryMin": "inválido", "likesWaitFrom": "inválido" }),
        vec![],
        Platform::default(),
    )
    .await;
    assert_eq!(app.cfg_num("retryMin"), 15.0);
    assert_eq!(app.cfg_num("likesWaitFrom"), t0);
    for value in [
        json!(-1),
        json!(1e100),
        json!(253_402_300_800_000_i64),
        json!(0.5),
        Value::Null,
    ] {
        let app = boot(
            json!({ "likesWaitFrom": value }),
            vec![empty()],
            Platform::default(),
        )
        .await;
        assert_eq!(
            app.cfg_num("likesWaitFrom"),
            0.0,
            "prazo inválido não gera timer em loop"
        );
    }
    // ok('espera salva, prazo vencido, troca de intervalo e validação de configurações / IPC')
}

#[tokio::test(start_paused = true)]
async fn late_clock_or_far_deadline() {
    let t0 = t0();
    let app = boot(
        json!({ "likesWaitFrom": t0 + MIN }),
        vec![],
        Platform::default(),
    )
    .await;
    assert_eq!(app.calls(), 0);
    assert_eq!(app.cfg_num("likesWaitFrom"), t0 + MIN);
    app.tick(16.0 * MIN).await;
    assert_eq!(
        app.calls(),
        1,
        "relógio atrasado preserva a espera e executa uma tentativa no prazo salvo"
    );
    let app = boot(
        json!({ "likesWaitFrom": 4_102_444_800_000_i64 }),
        vec![],
        Platform::default(),
    )
    .await;
    app.tick(MIN).await;
    assert_eq!(
        app.calls(),
        0,
        "prazo distante não causa estouro do setTimeout nem loop de tentativas"
    );
    // ok('relógio atrasado ou data distante: prazo preservado, sem rajada de tentativas')
}

#[tokio::test(start_paused = true)]
async fn suspend_resume() {
    let t0 = t0();
    let app = boot(
        json!({ "retryMin": 60, "likesWaitFrom": t0 - 10.0 * MIN }),
        vec![],
        Platform::default(),
    )
    .await;
    app.resume();
    app.resume();
    app.flush().await;
    assert_eq!(app.next(), t0 + 50.0 * MIN);
    app.clock.set_time(t0 + 3.0 * 60.0 * MIN);
    app.resume();
    app.resume();
    app.flush().await;
    assert_eq!(app.next(), app.now() + 15_000.0);
    app.tick(14_999.0).await;
    assert_eq!(app.calls(), 0);
    app.tick(1.0).await;
    assert_eq!(app.calls(), 1);
    // ok('suspender/retomar: conserva o prazo ou faz uma tentativa após 15 s, sem rajada')
}

const OLD: [&str; 9] = [
    "recheckMin",
    "waitFrom",
    "waitWhy",
    "admirerOn",
    "cycles",
    "admirers",
    "driver",
    "geoLat",
    "geoLon",
];

fn kept() -> Value {
    json!({ "enabled": true, "skipFirst": false, "autostart": false, "likesToday": 9, "likesTotal": 587, "matches": 5, "startMinimized": true })
}

#[tokio::test(start_paused = true)]
async fn migration_drops_removed_keys() {
    let t0 = t0();
    // 2. migração: tira recheckMin (e a espera salva dele) e as chaves do Admirador Secreto e do emulador sem perder o resto
    let mut cfg = kept();
    let extra = json!({ "recheckMin": 60, "waitFrom": t0 - 5.0 * MIN, "waitWhy": "Likes esgotados", "admirerOn": false, "cycles": 7, "admirers": 2, "driver": "android", "geoLat": "-23,5", "geoLon": "-46.6" });
    cfg.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    let app = boot(cfg, vec![], Platform::default()).await;
    let c = app.cfg();
    for k in OLD {
        assert!(c.get(k).is_none(), "{k} saiu do arquivo");
        assert!(app.ui().get(k).is_none(), "{k} fora do estado da tela");
    }
    for (k, v) in kept().as_object().unwrap() {
        assert_eq!(stringify(&c[k]), stringify(v), "{k} preservado");
    }
    assert_eq!(
        app.calls(),
        1,
        "abre como sempre (1 verificação), sem retomar espera antiga"
    );
    app.set("recheckMin", json!(15)).await;
    app.set("recheckMin", json!(30)).await;
    assert!(app.cfg().get("recheckMin").is_none(), "IPC antigo recusado");
    app.set("admirerOn", json!(true)).await;
    assert!(
        app.cfg().get("admirerOn").is_none(),
        "IPC do Admirador Secreto recusado"
    );
    app.set("driver", json!("android")).await;
    app.set("geoLat", json!("1")).await;
    assert!(
        app.cfg().get("driver").is_none() && app.cfg().get("geoLat").is_none(),
        "IPC do emulador recusado"
    );
    app.tick(MIN).await;
    assert_eq!(app.calls(), 1);
    let keys = |v: &Value| {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    };
    let app = boot(kept(), vec![], Platform::default()).await;
    assert_eq!(
        keys(&app.cfg()),
        keys(&c),
        "arquivo sem as chaves: nada muda"
    );
    // ok('migração: ... removidos, as outras 7 preferências e métricas iguais; IPCs antigos recusados')
}

#[tokio::test(start_paused = true)]
async fn check_now_one_round_at_a_time() {
    // "Verificar agora" antecipa a espera; cliques repetidos = uma rodada.
    let app = boot(json!({}), vec![], Platform::default()).await;
    let (done, p) = deferred();
    app.push(later(p));
    for _ in 0..5 {
        app.check();
    }
    app.flush().await;
    assert_eq!(app.calls(), 2, "5 cliques = 1 rodada");
    assert_eq!(app.code(), "running");
    done(rr(Reason::Out, 0));
    app.flush().await;
    assert_eq!(app.why(), "Likes esgotados");
    app.tick(MIN).await;
    assert_eq!(app.calls(), 2);
    app.check();
    app.flush().await;
    assert_eq!(app.calls(), 3, "Verificar agora de novo funciona");
    // ok('Verificar agora: 1 rodada por vez mesmo com 5 cliques; continua funcionando depois')
}

#[tokio::test(start_paused = true)]
async fn events_while_busy_do_not_run_alongside() {
    // 4. eventos enquanto ocupado não rodam junto
    let app = boot(json!({}), vec![], Platform::default()).await;
    let (done, p) = deferred();
    app.push(later(p));
    app.check();
    app.flush().await;
    app.resume();
    app.check();
    app.toggle(true);
    app.tick(MIN).await;
    assert_eq!(app.calls(), 2, "nada em paralelo");
    done(rr(Reason::Empty, 0));
    app.flush().await;
    app.tick(H24).await;
    assert_eq!(app.calls(), 2);
    // ok('resume/check/toggle durante uma rodada: ignorados, sem rodada concorrente')
}

#[tokio::test(start_paused = true)]
async fn pause_resume_restart_keep_the_deadline() {
    let t0 = t0();
    // Pausar cancela o timer, mas conserva o prazo ao reativar ou reiniciar.
    let app = boot(json!({}), vec![], Platform::default()).await;
    app.toggle(false);
    app.flush().await;
    assert_eq!(app.code(), "paused");
    assert_eq!(app.cfg()["enabled"], json!(false));
    app.check();
    app.resume();
    app.tick(MIN).await;
    assert_eq!(app.calls(), 1, "pausado não roda");
    app.toggle(true);
    app.flush().await;
    assert_eq!(app.calls(), 1, "reativar conserva a espera");
    assert_eq!(app.next(), t0 + 15.0 * MIN);
    app.tick(14.0 * MIN).await;
    assert_eq!(app.calls(), 2);
    let app = boot(
        json!({ "enabled": false, "likesWaitFrom": t0, "retryMin": 60 }),
        vec![],
        Platform::default(),
    )
    .await;
    assert_eq!(app.calls(), 0);
    assert_eq!(app.code(), "paused");
    app.toggle(true);
    app.flush().await;
    assert_eq!(app.calls(), 0);
    assert_eq!(app.next(), t0 + 60.0 * MIN);
    let app = boot(json!({}), vec![empty()], Platform::default()).await;
    let (done, p) = deferred();
    app.results(vec![later(p)]);
    app.check();
    app.flush().await;
    app.toggle(false);
    app.flush().await;
    done(rr(Reason::Out, 0));
    app.flush().await;
    assert_eq!(app.code(), "paused", "pausou durante a rodada");
    let app = boot(json!({}), vec![empty()], Platform::default()).await;
    let (done, p) = deferred();
    app.results(vec![later(p)]);
    app.check();
    app.flush().await;
    app.toggle(false);
    app.toggle(true);
    app.flush().await;
    done(rr(Reason::Stopped, 0));
    app.flush().await;
    app.tick(0.0).await;
    assert_eq!(app.calls(), 3, "pausar+religar rápido: recomeça");
    assert_eq!(app.why(), "Likes esgotados");
    // ok('pausar/reativar/reiniciar respeitam o prazo; pausar durante a rodada e religar rápido funcionam')
}

#[tokio::test(start_paused = true)]
async fn offline_backoff_error_stuck_login() {
    let t0 = t0();
    // 6. temporizadores preservados: sem conexão (5 s → 1/min), erro (1 min), tela travada (recarrega 1×, depois 5 min), login (5 s)
    let app = boot(json!({}), vec![], Platform::default()).await;
    app.s.page.fail.set(true);
    app.check();
    app.flush().await;
    assert_eq!(app.why(), "Sem conexão com o Tinder");
    let mut gaps = vec![];
    for _ in 0..6 {
        let g = app.next() - app.now();
        gaps.push(g / 1000.0);
        app.tick(g).await;
    }
    assert_eq!(
        gaps,
        [5.0, 15.0, 30.0, 60.0, 60.0, 60.0],
        "espaça: 5 s, 15 s, 30 s, depois 1/min"
    );
    assert_eq!(app.calls(), 1, "sem rodar o motor");
    app.s.page.fail.set(false);
    app.tick(60_000.0).await;
    assert_eq!(app.calls(), 2, "voltou: 1 rodada");
    app.s.page.fail.set(true);
    app.check();
    app.flush().await;
    assert_eq!(app.next() - app.now(), 5000.0, "queda nova recomeça em 5 s");
    for ms in [5000.0, 15000.0, 30000.0, 60000.0] {
        app.tick(ms).await;
    }
    app.clock.set_time(app.now() + 3.0 * 60.0 * MIN); // dormiu 3 h
    app.resume();
    app.resume();
    app.flush().await;
    assert_eq!(app.next(), app.now() + 15_000.0);
    app.s.page.fail.set(false);
    app.tick(15_000.0).await;
    assert_eq!(app.calls(), 3, "rede voltou: 1 rodada");
    assert_eq!(app.next(), app.now() + 15.0 * MIN);
    app.tick(MIN).await;
    assert_eq!(app.calls(), 3);

    // sem internet com a página vinda do cache (service worker): a API falha por rede → para a rodada e espaça as tentativas
    let app = boot(json!({}), vec![], Platform::default()).await;
    // agenda.js also checks the webRequest filter {urls: tinder.com/*, *.tinder.com/*, *.gotinder.com/*}: that
    // filter is the host's (iface HostEvent::NetError); the core only sees the errors of those URLs
    let down = || {
        Run(Box::new(|s: &Shared| {
            let _ =
                s.tx.send(HostEvent::NetError("net::ERR_INTERNET_DISCONNECTED".into())); // url: https://api.gotinder.com/v2/recs/core
            Box::pin(async { Ok(rr(Reason::Stopped, 0)) })
        }))
    };
    app.results((0..6).map(|_| down()).collect());
    app.check();
    app.flush().await;
    assert_eq!(app.why(), "Sem conexão com o Tinder", "reconheceu a queda");
    let mut g2 = vec![];
    for _ in 0..5 {
        let g = app.next() - app.now();
        g2.push(g / 1000.0);
        app.tick(g).await;
    }
    assert_eq!(
        g2,
        [5.0, 15.0, 30.0, 60.0, 60.0],
        "mesmo com a página carregando, espaça as tentativas"
    );
    assert_eq!(app.calls(), 7);
    app.results(vec![out()]);
    app.tick(60_000.0).await;
    assert_eq!(app.why(), "Likes esgotados", "rede voltou: segue normal");
    app.results(vec![down()]);
    app.check();
    app.flush().await;
    assert_eq!(app.next() - app.now(), 5000.0, "queda nova recomeça em 5 s");
    // erros que não são queda de rede não param nada
    let errors = Run(Box::new(|s: &Shared| {
        for error in [
            "net::ERR_ABORTED",
            "net::ERR_BLOCKED_BY_CLIENT",
            "net::ERR_HTTP2_PROTOCOL_ERROR",
        ] {
            let _ = s.tx.send(HostEvent::NetError(error.into())); // url: https://api.gotinder.com/x
        }
        Box::pin(async { Ok(rr(Reason::Out, 0)) })
    }));
    let app = boot(json!({}), vec![errors], Platform::default()).await;
    assert_eq!(app.why(), "Likes esgotados");
    // ok('sem internet com página do cache: API falhando por rede para a rodada → "Sem conexão" 5 s/15 s/30 s/1 min; ERR_ABORTED etc. não contam')

    let app = boot(
        json!({}),
        vec![Run(Box::new(|_| {
            Box::pin(async { Err("falhou".to_string()) })
        }))],
        Platform::default(),
    )
    .await;
    assert!(app.text().starts_with("Erro: falhou"));
    assert_eq!(app.next(), t0 + MIN);
    app.tick(MIN).await;
    assert_eq!(app.calls(), 2, "erro: tenta de novo em 1 min");
    let app = boot(
        json!({}),
        vec![
            Ready(rr(Reason::Stuck, 3)),
            Ready(rr(Reason::Reload, 1)),
            Ready(rr(Reason::Stuck, 0)),
            Ready(rr(Reason::Stuck, 0)),
        ],
        Platform::default(),
    )
    .await;
    assert_eq!(app.calls(), 4);
    assert!(app.text().starts_with("Tela inesperada"));
    assert_eq!(app.next(), t0 + 5.0 * MIN);
    app.tick(5.0 * MIN).await;
    assert_eq!(app.calls(), 5);
    let app = boot(json!({ "enabled": false }), vec![], Platform::default()).await; // login: sem página /app, espera o login sem recarregar
    *app.s.page.url.borrow_mut() = "https://tinder.com/".into();
    app.toggle(true);
    app.flush().await;
    assert_eq!(app.code(), "login");
    assert_eq!(app.calls(), 0);
    *app.s.page.url.borrow_mut() = "https://tinder.com/app/recs".into();
    app.tick(5000.0).await;
    assert_eq!(app.calls(), 1, "logou: começa");
    // ok('preservados: sem conexão 5 s/15 s/30 s/1 min (+ retorno da suspensão), erro 1 min, tela travada 1 recarga + 5 min, login a cada 5 s')
}

#[tokio::test(start_paused = true)]
async fn windows_task() {
    let t0 = t0();
    for retry_min in [15.0, 30.0, 60.0, 120.0, 360.0] {
        let app = boot(
            json!({ "retryMin": retry_min }),
            vec![],
            Platform {
                hidden: true,
                ..packaged()
            },
        )
        .await;
        assert_eq!(*app.s.scheduled.borrow(), [t0 + retry_min * MIN]);
        assert_eq!(app.quits(), 1, "só fecha após confirmação do agendamento");
    }
    let reject: ScheduleFn = Rc::new(|_, _| Box::pin(async { Err("falha simulada".to_string()) }));
    let app = boot(
        json!({}),
        vec![],
        Platform {
            hidden: true,
            schedule: Some(reject),
            ..packaged()
        },
    )
    .await;
    assert_eq!(
        app.quits(),
        0,
        "sem agendamento confirmado, permanece aberto"
    );
    assert!(app.text().contains("Não consegui agendar"));
    assert_eq!(
        app.next(),
        t0 + 15.0 * MIN,
        "timer interno continua funcionando"
    );
    let app = boot(
        json!({}),
        vec![],
        Platform {
            hidden: true,
            write_fails: true,
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.quits(), 0);
    assert!(
        app.s.scheduled.borrow().is_empty(),
        "não fecha nem agenda se o prazo não foi salvo"
    );
    let (registered, registering) = deferred::<()>();
    let registering = RefCell::new(Some(registering));
    let wait: ScheduleFn = Rc::new(move |_, _| {
        let p = registering.borrow_mut().take();
        Box::pin(async move {
            if let Some(p) = p {
                p.await;
            }
            Ok(Value::Null)
        })
    });
    let app = boot(
        json!({}),
        vec![],
        Platform {
            hidden: true,
            schedule: Some(wait),
            ..packaged()
        },
    )
    .await;
    app.toggle(false);
    app.flush().await;
    registered(());
    app.flush().await;
    assert_eq!(
        app.quits(),
        0,
        "pausar durante o registro impede o fechamento automático"
    );
    assert_eq!(app.code(), "paused");
    assert!(app.s.cancelled.get() >= 2);
    for (name, change) in [("changing", true), ("restarting", false)] {
        let (finish, first) = deferred::<()>();
        let first = RefCell::new(Some(first));
        // schedule: () => app.scheduled.length === 1 ? <deferred>.p : Promise.resolve()
        let second_resolves: ScheduleFn = Rc::new(move |s, _| {
            let p = if s.scheduled.borrow().len() == 1 {
                first.borrow_mut().take()
            } else {
                None
            };
            Box::pin(async move {
                if let Some(p) = p {
                    p.await;
                }
                Ok(Value::Null)
            })
        });
        let app = boot(
            json!({}),
            vec![],
            Platform {
                hidden: true,
                schedule: Some(second_resolves),
                ..packaged()
            },
        )
        .await;
        if change {
            app.set("retryMin", json!(60)).await;
        } else {
            app.toggle(false);
            app.toggle(true);
            app.flush().await;
        }
        finish(());
        app.flush().await;
        if change {
            assert_eq!(
                *app.s.scheduled.borrow(),
                [t0 + 15.0 * MIN, t0 + 60.0 * MIN],
                "{name}"
            );
            assert_eq!(app.next(), t0 + 60.0 * MIN);
        } else {
            assert_eq!(app.s.scheduled.borrow().len(), 2);
            assert_eq!(app.next(), t0 + 15.0 * MIN);
        }
        assert_eq!(app.quits(), 0);
    }
    let app = boot(
        json!({ "retryMin": 30, "likesWaitFrom": t0 - 10.0 * MIN }),
        vec![],
        packaged(),
    )
    .await;
    assert_eq!(
        app.quits(),
        0,
        "abertura manual durante a espera permite alterar ajustes"
    );
    assert_eq!(*app.s.scheduled.borrow(), [t0 + 20.0 * MIN]);
    app.set("retryMin", json!(60)).await;
    assert_eq!(app.s.scheduled.borrow().last(), Some(&(t0 + 50.0 * MIN)));
    app.toggle(false);
    app.flush().await;
    assert!(app.s.cancelled.get() >= 1);
    let app = boot(
        json!({ "enabled": false, "likesWaitFrom": t0 }),
        vec![],
        Platform {
            args: retry_at(t0 + 15.0 * MIN),
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.quits(), 1);
    assert_eq!(app.calls(), 0);
    assert!(
        app.s.win.get().is_none(),
        "agendamento cancelado não abre janela"
    );
    let app = boot(
        json!({ "likesWaitFrom": t0 - 5.0 * MIN }),
        vec![],
        Platform {
            args: retry_at(t0 - MIN),
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.quits(), 1);
    assert_eq!(
        app.calls(),
        0,
        "tarefa antiga respeita o prazo salvo e encerra durante a espera"
    );
    assert_eq!(app.s.scheduled.borrow().last(), Some(&(t0 + 10.0 * MIN)));
    let app = boot(
        json!({ "likesWaitFrom": t0 - 30.0 * MIN }),
        vec![],
        Platform {
            args: retry_at(t0 - MIN),
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.quits(), 1);
    assert_eq!(
        app.calls(),
        1,
        "registro interrompido retoma a tentativa já vencida"
    );
    let app = boot(
        json!({ "likesWaitFrom": t0 - 30.0 * MIN, "startMinimized": false }),
        vec![empty()],
        Platform {
            args: retry_at(t0 - 15.0 * MIN),
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.calls(), 1);
    assert!(
        app.s.minimized.get(),
        "retomada abre minimizada mesmo com a preferência desligada"
    );
    assert_eq!(
        app.quits(),
        0,
        "sem limite de likes, não encerra automaticamente"
    );
    // ok('Windows: cinco prazos, falha no agendamento, pausa concorrente, reprogramação, tarefa antiga e abertura minimizada')
}

#[tokio::test(start_paused = true)]
async fn shutdown_interrupted_attempt_restarts_corrupt_files_session_end() {
    let t0 = t0();
    let old_wait = t0 - 30.0 * MIN;
    let (_active_done, active) = deferred();
    let app = boot(
        json!({ "retryMin": 15, "likesWaitFrom": old_wait, "likesTotal": 2868 }),
        vec![later(active)],
        Platform {
            args: retry_at(t0 - 15.0 * MIN),
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.cfg_num("likesWaitFrom"), old_wait);
    assert_eq!(
        app.s.cancelled.get(),
        0,
        "tentativa em andamento mantém a tarefa e o prazo no disco"
    );
    for _ in 0..5 {
        app.emit(HostEvent::SecondInstance(retry_at(t0 - MIN)));
    }
    app.resume();
    app.flush().await;
    assert_eq!(
        app.calls(),
        1,
        "inicialização do Windows e tarefa simultâneas não duplicam a tentativa"
    );
    let interrupted = app.cfg();
    let app = boot(
        interrupted,
        vec![empty()],
        Platform {
            args: retry_at(t0 - MIN),
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.calls(), 1);
    assert_eq!(app.cfg_num("likesWaitFrom"), 0.0);
    assert_eq!(app.cfg_num("likesTotal"), 2868.0);
    assert!(
        app.s.cancelled.get() > 0,
        "só consome a tarefa depois de concluir a tentativa"
    );
    app.emit(HostEvent::SecondInstance(retry_at(t0 - MIN)));
    app.flush().await;
    assert_eq!(
        app.calls(),
        1,
        "um segundo disparo antigo é ignorado depois de concluir a tentativa"
    );
    let app = boot(
        json!({ "likesWaitFrom": t0, "retryMin": 360 }),
        vec![],
        Platform {
            args: vec!["--hidden".into()],
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.calls(), 0);
    assert_eq!(
        app.quits(),
        1,
        "reinício antes do prazo agenda e encerra, sem ficar rodando"
    );
    assert_eq!(app.s.scheduled.borrow().last(), Some(&(t0 + 360.0 * MIN)));
    let mut recovered = kept();
    recovered.as_object_mut().unwrap().extend(
        json!({ "enabled": false, "retryMin": 360, "likesWaitFrom": t0 })
            .as_object()
            .unwrap()
            .clone(),
    );
    let app = boot(
        json!({}),
        vec![],
        Platform {
            raw: Some("{".into()),
            backup: Some(stringify(&recovered)),
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.calls(), 0);
    assert_eq!(app.ui()["enabled"], json!(false));
    assert_eq!(app.cfg_num("likesTotal"), 587.0);
    assert!(app.logs().iter().any(|l| l.contains("recuperadas")));
    let mut enabled = recovered.clone();
    enabled["enabled"] = json!(true);
    let app = boot(
        json!({}),
        vec![],
        Platform {
            raw: Some("{".into()),
            backup: Some(stringify(&enabled)),
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.calls(), 0);
    assert_eq!(
        app.cfg()["enabled"],
        json!(false),
        "a cópia antiga não pode desfazer uma possível pausa mais recente"
    );
    let app = boot(
        json!({ "enabled": true }),
        vec![],
        Platform {
            temporary: Some(stringify(&recovered)),
            args: retry_at(t0 + 360.0 * MIN),
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.calls(), 0);
    assert_eq!(app.quits(), 1);
    assert_eq!(
        app.cfg()["enabled"],
        json!(false),
        "pausa salva no temporário é recuperada após uma interrupção"
    );
    let app = boot(
        json!({}),
        vec![],
        Platform {
            raw: Some("{".into()),
            backup: Some("null".into()),
            temporary: Some("[]".into()),
            ..packaged()
        },
    )
    .await;
    assert_eq!(app.calls(), 0);
    assert_eq!(app.ui()["enabled"], json!(false));
    assert!(
        app.logs()
            .iter()
            .any(|l| l.contains("inválidas; Automático pausado"))
    );
    assert!(app.s.dir.join("spark.json.corrompido").exists());
    let app = boot(
        json!({ "enabled": false, "likesTotal": 2868 }),
        vec![],
        Platform::default(),
    )
    .await;
    app.emit(HostEvent::SessionEnd);
    app.flush().await;
    assert_eq!(app.cfg_num("likesTotal"), 2868.0);
    assert_eq!(app.cfg()["enabled"], json!(false));
    // ok('desligamento: tentativa interrompida, reinício antes/depois do prazo, arquivos corrompidos, pausa recuperada e fim de sessão')
}
