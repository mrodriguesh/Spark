//! Host: the UI thread. Owns the tao window, the UI web view (index.html, byte-identical), the Tinder web view
//! (separate `tinder` profile), popups, tray and the Windows hooks; forwards everything to the core (logic
//! thread) as `HostEvent`s and executes the core's requests (`Cmd`). Evidence for each mechanism:
//!
#[cfg(feature = "test-hooks")]
mod control;
mod migrate;
pub mod os;
mod single;
mod web;

use std::{
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use serde_json::{Value, json};
use tao::{
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget},
    platform::windows::{IconExtWindows, WindowExtWindows},
    window::{Icon, ResizeDirection, Window, WindowBuilder},
};
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowPlacement, GetWindowRect, IsIconic, IsWindowVisible,
        IsZoomed, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOW, SW_SHOWNOACTIVATE,
        SetForegroundWindow, ShowWindow, WINDOWPLACEMENT,
    },
};
use wry::WebView;

use crate::iface::{Fut, Host, HostEvent, LoginItem, Page, Placement, WinState};

/// main.js:30
pub const HOME: &str = "https://tinder.com/app/recs";
const INDEX_HTML: &[u8] = include_bytes!("../../ui/index.html");
const ICON_PNG: &[u8] = include_bytes!("../../assets/icon.png");
const ICON_ICO: &[u8] = include_bytes!("../../assets/icon.ico");

/// Requests from the core (logic thread) to the UI thread.
pub enum Cmd {
    CreateWindow(Placement),
    UiMessage(Value),
    Tray {
        tooltip: String,
        status: String,
        enabled: bool,
    },
    Notify(String),
    Window(mpsc::Sender<WinState>),
    Minimize,
    ToggleMaximize,
    FromTray,
    Quit,
    Geo(Option<(f64, f64, f64)>),
    Eval {
        script: String,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    Key {
        key_code: &'static str,
        reply: oneshot::Sender<()>,
    },
    Load {
        url: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Url(mpsc::Sender<String>),
    // UI-thread internal
    Ipc(String),
    TrayIcon(TrayIconEvent),
    Menu(MenuEvent),
    Web(web::WebCmd),
    CoreExited,
    /// test-hooks control port request (control.rs)
    Control(Value, mpsc::Sender<Value>),
}

/// Paths and flags shared by both threads.
#[derive(Clone)]
pub struct Env {
    pub exe: String,
    pub user_data: PathBuf,
    pub assets: PathBuf,
    pub packaged: bool,
    pub attached: Arc<AtomicBool>,
    pub destroyed: Arc<AtomicBool>,
}

static USER_DATA: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// test-hooks setting: the env var, else the same key in `<userData>\spark-test.json` (a launch by the Task
/// Scheduler inherits no environment; the original test used `retry-test.json` the same way). Release: never read.
pub fn test_setting(name: &str) -> Option<String> {
    if !cfg!(feature = "test-hooks") {
        return None;
    }
    std::env::var(name).ok().or_else(|| {
        let text = std::fs::read_to_string(USER_DATA.get()?.join("spark-test.json")).ok()?;
        let v: Value = serde_json::from_str(&text).ok()?;
        v[name].as_str().map(String::from)
    })
}

fn arg_value(argv: &[String], name: &str) -> Option<String> {
    argv.iter()
        .find_map(|a| a.strip_prefix(&format!("--{name}=")).map(String::from))
}

/// The process entry point (main.rs).
pub fn run() {
    let argv: Vec<String> = std::env::args().collect();
    // Electron DIR_USER_DATA: %APPDATA%\<productName>, overridden verbatim by --user-data-dir
    let user_data = arg_value(&argv, "user-data-dir")
        .unwrap_or_else(|| format!("{}\\Spark", std::env::var("APPDATA").unwrap_or_default()));
    let _ = USER_DATA.set(PathBuf::from(&user_data));
    os::set_aumid();
    let _guard = match single::acquire(&user_data) {
        single::Lock::First(g) => g,
        single::Lock::Notified | single::Lock::Failed => std::process::exit(0),
    };
    let exe = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let assets = std::env::temp_dir().join(format!("Spark-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&assets);
    let _ = std::fs::write(assets.join("icon.png"), ICON_PNG);
    let _ = std::fs::write(assets.join("icon.ico"), ICON_ICO);
    let packaged = if cfg!(feature = "test-hooks") {
        test_setting("SPARK_TEST_PACKAGED").is_some_and(|v| v == "1")
    } else {
        !exe.to_ascii_lowercase().contains("\\target\\")
    };
    let env = Env {
        exe,
        user_data: PathBuf::from(&user_data),
        assets,
        packaged,
        attached: Arc::default(),
        destroyed: Arc::default(),
    };

    let event_loop = EventLoopBuilder::<Cmd>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel::<HostEvent>();
    single::set_sink(events_tx.clone());
    os::power_window(events_tx.clone());
    {
        let p = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(Cmd::TrayIcon(e));
        }));
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = p.send_event(Cmd::Menu(e));
        }));
    }

    #[cfg(feature = "test-hooks")]
    control::start(proxy.clone());
    let core_env = env.clone();
    let core_proxy = proxy.clone();
    std::thread::spawn(move || {
        let host: Rc<dyn Host> = Rc::new(HostRemote {
            proxy: core_proxy.clone(),
            env: core_env.clone(),
        });
        crate::core::run_core(argv, core_env.user_data.clone(), host, events_rx);
        let _ = core_proxy.send_event(Cmd::CoreExited);
    });

    let mut ui = UiState {
        env: Some(env),
        proxy: Some(proxy.clone()),
        events: Some(events_tx),
        ..Default::default()
    };
    event_loop.run(move |event, target, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(cmd) => {
                if ui.handle(cmd, target) {
                    single::shutting_down();
                    ui.teardown();
                    *flow = ControlFlow::Exit;
                }
            }
            Event::WindowEvent {
                event, window_id, ..
            } => ui.window_event(event, window_id),
            _ => {}
        }
    });
}

// ---------------------------------------------------------------- logic-thread side

/// `Host` for the core: forwards to the UI thread.
struct HostRemote {
    proxy: EventLoopProxy<Cmd>,
    env: Env,
}

impl Host for HostRemote {
    fn create_window(&self, placement: Placement) {
        let _ = self.proxy.send_event(Cmd::CreateWindow(placement));
    }
    fn send_state(&self, snapshot: &Value) {
        let _ = self
            .proxy
            .send_event(Cmd::UiMessage(json!({ "ch": "state", "value": snapshot })));
    }
    fn reply_state(&self, id: u64, snapshot: &Value) {
        let _ = self
            .proxy
            .send_event(Cmd::UiMessage(json!({ "id": id, "value": snapshot })));
    }
    fn set_tray(&self, tooltip: &str, status_text: &str, enabled: bool) {
        let _ = self.proxy.send_event(Cmd::Tray {
            tooltip: tooltip.into(),
            status: status_text.into(),
            enabled,
        });
    }
    fn notify(&self, body: &str) {
        let _ = self.proxy.send_event(Cmd::Notify(body.into()));
    }
    fn window(&self) -> WinState {
        let (tx, rx) = mpsc::channel();
        let _ = self.proxy.send_event(Cmd::Window(tx));
        rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default()
    }
    fn minimize(&self) {
        let _ = self.proxy.send_event(Cmd::Minimize);
    }
    fn toggle_maximize(&self) {
        let _ = self.proxy.send_event(Cmd::ToggleMaximize);
    }
    fn from_tray(&self) {
        let _ = self.proxy.send_event(Cmd::FromTray);
    }
    fn quit(&self) {
        let _ = self.proxy.send_event(Cmd::Quit);
    }
    fn page(&self) -> Option<Rc<dyn Page>> {
        self.env.attached.load(Ordering::SeqCst).then(|| {
            Rc::new(PageRemote {
                proxy: self.proxy.clone(),
                destroyed: self.env.destroyed.clone(),
            }) as Rc<dyn Page>
        })
    }
    fn packaged(&self) -> bool {
        self.env.packaged
    }
    fn login_item(&self) -> LoginItem {
        os::login_item(&self.env.exe)
    }
    fn set_login_item(&self, open_at_login: bool, enabled: Option<bool>) {
        os::set_login_item(&self.env.exe, open_at_login, enabled);
    }
    fn set_geolocation(&self, geo: Option<(f64, f64, f64)>) {
        let _ = self.proxy.send_event(Cmd::Geo(geo));
    }
}

/// `Page` for the core: the Tinder web view, through the UI thread.
struct PageRemote {
    proxy: EventLoopProxy<Cmd>,
    destroyed: Arc<AtomicBool>,
}

impl Page for PageRemote {
    fn eval(&self, script: &str) -> Fut<'_, Result<Value, String>> {
        let (reply, rx) = oneshot::channel();
        let sent = self
            .proxy
            .send_event(Cmd::Eval {
                script: script.into(),
                reply,
            })
            .is_ok();
        Box::pin(async move {
            if sent {
                rx.await
                    .unwrap_or_else(|_| Err("a página do Tinder não respondeu".into()))
            } else {
                Err("a página do Tinder não respondeu".into())
            }
        })
    }
    fn key(&self, key_code: &'static str) -> Fut<'_, ()> {
        let (reply, rx) = oneshot::channel();
        let _ = self.proxy.send_event(Cmd::Key { key_code, reply });
        Box::pin(async move {
            let _ = rx.await;
        })
    }
    fn load(&self, url: &str) -> Fut<'_, Result<(), String>> {
        let (reply, rx) = oneshot::channel();
        let _ = self.proxy.send_event(Cmd::Load {
            url: url.into(),
            reply,
        });
        Box::pin(async move { rx.await.unwrap_or_else(|_| Err("ERR_ABORTED".into())) })
    }
    fn url(&self) -> String {
        let (tx, rx) = mpsc::channel();
        let _ = self.proxy.send_event(Cmd::Url(tx));
        rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default()
    }
    fn destroyed(&self) -> bool {
        self.destroyed.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------- UI-thread side

struct TrayMenu {
    open: MenuItem,
    status: MenuItem,
    toggle: MenuItem,
    check: MenuItem,
    quit: MenuItem,
}

#[derive(Default)]
struct UiState {
    env: Option<Env>,
    proxy: Option<EventLoopProxy<Cmd>>,
    events: Option<UnboundedSender<HostEvent>>,
    window: Option<Window>,
    ui: Option<WebView>,
    web: Option<web::TinderWeb>,
    tray: Option<TrayIcon>,
    menu: Option<TrayMenu>,
    tray_bounds: Option<(PhysicalPosition<i32>, PhysicalSize<u32>)>,
    maximized: bool,
    focused: Option<bool>,
}

impl UiState {
    fn env(&self) -> Option<&Env> {
        self.env.as_ref()
    }
    fn emit(&self, e: HostEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.send(e);
        }
    }
    fn hwnd(&self) -> Option<HWND> {
        self.window.as_ref().map(|w| HWND(w.hwnd() as _))
    }
    fn to_ui(&self, msg: &Value) {
        if let Some(ui) = &self.ui {
            // JS key order and lone surrogates as JSON.stringify would send them (store unit)
            let _ = ui.evaluate_script(&format!(
                "window.__sparkHost({})",
                spark_store::port::stringify(msg)
            ));
        }
    }

    /// Returns true when the app must exit.
    fn handle(&mut self, cmd: Cmd, target: &EventLoopWindowTarget<Cmd>) -> bool {
        match cmd {
            Cmd::CreateWindow(p) => self.create(p, target),
            Cmd::UiMessage(m) => self.to_ui(&m),
            Cmd::Tray {
                tooltip,
                status,
                enabled,
            } => self.set_tray(&tooltip, &status, enabled),
            Cmd::Notify(body) => {
                if let Some(env) = self.env() {
                    os::toast(&body, &env.assets.join("icon.png"));
                }
            }
            Cmd::Window(reply) => {
                let _ = reply.send(self.win_state());
            }
            Cmd::Minimize => self.show(SW_MINIMIZE),
            Cmd::ToggleMaximize => {
                if let Some(w) = &self.window {
                    let _ = w;
                    self.toggle_zoom();
                }
            }
            Cmd::FromTray => self.restore_from_tray(),
            Cmd::Quit | Cmd::CoreExited => return true,
            Cmd::Geo(g) => {
                if let Some(web) = &mut self.web {
                    web.set_geo(g);
                }
            }
            Cmd::Eval { script, reply } => match &self.web {
                Some(web) => web.eval(&script, reply),
                None => {
                    let _ = reply.send(Err("a página do Tinder não respondeu".into()));
                }
            },
            Cmd::Key { key_code, reply } => {
                if let Some(web) = &self.web {
                    web.key(key_code);
                }
                let _ = reply.send(());
            }
            Cmd::Load { url, reply } => match &mut self.web {
                Some(web) => web.load(&url, reply),
                None => {
                    let _ = reply.send(Err("ERR_ABORTED".into()));
                }
            },
            Cmd::Url(reply) => {
                let _ = reply.send(self.web.as_ref().map(|w| w.url()).unwrap_or_default());
            }
            Cmd::Ipc(body) => self.ipc(&body),
            Cmd::TrayIcon(e) => {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = e
                {
                    self.emit(HostEvent::TrayClick);
                }
            }
            Cmd::Menu(e) => {
                if let Some(m) = &self.menu {
                    let id = e.id();
                    let ev = if id == m.open.id() {
                        Some(HostEvent::TrayClick)
                    } else if id == m.toggle.id() {
                        Some(HostEvent::TrayToggle)
                    } else if id == m.check.id() {
                        Some(HostEvent::TrayCheck)
                    } else if id == m.quit.id() {
                        Some(HostEvent::TrayQuit)
                    } else {
                        None
                    };
                    if let Some(ev) = ev {
                        self.emit(ev);
                    }
                }
            }
            Cmd::Web(c) => {
                if let Some(web) = &mut self.web {
                    web.on_cmd(c, target);
                }
            }
            Cmd::Control(req, reply) => {
                let _ = reply.send(self.control(&req));
            }
        }
        false
    }

    /// test-hooks requests (control.rs); in release builds nothing sends them.
    fn control(&mut self, req: &Value) -> Value {
        let s = self.win_state();
        match req["cmd"].as_str().unwrap_or("") {
            "info" => {
                let (w, h, sf) = self
                    .window
                    .as_ref()
                    .map(|w| {
                        (
                            w.inner_size().width,
                            w.inner_size().height,
                            w.scale_factor(),
                        )
                    })
                    .unwrap_or((0, 0, 1.0));
                let pos = self.window_rect();
                json!({ "min": s.minimized, "visible": s.visible, "x": s.x, "y": pos.1, "outerX": pos.0, "w": w, "h": h, "scale": sf, "maximized": self.maximized, "pid": std::process::id(), "browserPid": self.web.as_ref().map(|w| w.browser_pid()) })
            }
            "size" => {
                if let (Some(w), Some(x), Some(y)) =
                    (&self.window, req["w"].as_f64(), req["h"].as_f64())
                {
                    w.set_inner_size(LogicalSize::new(x, y));
                }
                json!(true)
            }
            "topmost" => {
                // real-input tests: keep the window above the terminal (Windows' foreground lock)
                if let Some(h) = self.hwnd() {
                    use windows::Win32::UI::WindowsAndMessaging::{
                        HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
                    };
                    let after = if req["on"].as_bool().unwrap_or(true) {
                        HWND_TOPMOST
                    } else {
                        HWND_NOTOPMOST
                    };
                    // SAFETY: our own window; only z-order changes.
                    let _ = unsafe {
                        SetWindowPos(h, Some(after), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE)
                    };
                }
                json!(true)
            }
            "minimize" => {
                self.show(SW_MINIMIZE);
                json!(true)
            }
            "restore" => {
                self.show(SW_RESTORE);
                if let Some(w) = &self.window {
                    w.set_focus();
                }
                json!(true)
            }
            "close" => {
                self.emit(HostEvent::WindowClose);
                json!(true)
            }
            "second-instance" => {
                let argv = req["argv"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                self.emit(HostEvent::SecondInstance(argv));
                json!(true)
            }
            "tinder-load" => {
                if let (Some(web), Some(url)) = (&mut self.web, req["url"].as_str()) {
                    let (reply, _) = oneshot::channel();
                    web.load(url, reply);
                }
                json!(true)
            }
            #[cfg(feature = "test-hooks")]
            "toasts" => json!(os::TOASTS.lock().map(|g| g.clone()).unwrap_or_default()),
            #[cfg(feature = "test-hooks")]
            "capture" => match (self.hwnd(), req["path"].as_str()) {
                (Some(h), Some(path)) => json!(os::capture(h, std::path::Path::new(path)).is_ok()),
                _ => json!(false),
            },
            other => json!({ "error": format!("unknown cmd {other}") }),
        }
    }

    fn create(&mut self, placement: Placement, target: &EventLoopWindowTarget<Cmd>) {
        let Some(env) = self.env.clone() else { return };
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let icon = Icon::from_path(env.assets.join("icon.ico"), None).ok();
        let Ok(window) = WindowBuilder::new()
            .with_title("Spark")
            .with_decorations(false)
            .with_visible(false)
            .with_window_icon(icon.clone())
            .with_inner_size(LogicalSize::new(1320.0, 860.0))
            .with_min_inner_size(LogicalSize::new(1100.0, 700.0))
            .build(target)
        else {
            return;
        };
        center(&window);
        migrate::run(
            &env.user_data,
            HWND(window.hwnd() as _),
            &env.assets.join("icon.png"),
        ); //before the Tinder profile opens
        let (ui, web) = match web::build(&window, &env, proxy.clone(), self.events.clone(), icon) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("web views: {e}");
                return;
            }
        };
        self.window = Some(window);
        self.ui = Some(ui);
        self.web = Some(web);
        env.attached.store(true, Ordering::SeqCst);
        self.emit(HostEvent::Attached);
        if let Some(web) = &mut self.web {
            let (reply, _) = oneshot::channel();
            web.load(HOME, reply); // <webview src=TINDER> loads on attach
        }
        match placement {
            Placement::Minimized => self.show(SW_MINIMIZE),
            Placement::Tray => self.place_in_tray(),
            Placement::Shown => {
                self.show(SW_SHOW);
                if let Some(w) = &self.window {
                    w.set_focus();
                }
            }
        }
        self.build_tray();
    }

    fn build_tray(&mut self) {
        let Some(env) = self.env() else { return };
        let menu = Menu::new();
        let m = TrayMenu {
            open: MenuItem::new("Abrir Spark", true, None),
            status: MenuItem::new("Iniciando…", false, None),
            toggle: MenuItem::new("Pausar", true, None),
            check: MenuItem::new("Verificar agora", true, None),
            quit: MenuItem::new("Sair", true, None),
        };
        let _ = menu.append_items(&[
            &m.open,
            &m.status,
            &PredefinedMenuItem::separator(),
            &m.toggle,
            &m.check,
            &PredefinedMenuItem::separator(),
            &m.quit,
        ]);
        let icon = tray_icon::Icon::from_path(env.assets.join("icon.ico"), Some((16, 16))).ok();
        let mut b = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false); // main.js creates the tray without a tooltip
        if let Some(i) = icon {
            b = b.with_icon(i);
        }
        match b.build() {
            Ok(t) => {
                self.tray = Some(t);
                self.menu = Some(m);
            }
            // main.js:460-462 logs it; the core cannot see this failure, so it is printed only
            // main.js:460-462 logs it; the core never sees this failure, so the host appends the same line
            Err(e) => {
                if let Some(env) = self.env() {
                    use std::io::Write;
                    let ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as f64)
                        .unwrap_or(0.0);
                    let line = format!(
                        "{}  Bandeja do sistema não pôde ser inicializada: {e}
",
                        crate::core::clock::log_stamp(ms)
                    );
                    let _ = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(env.user_data.join("spark.log"))
                        .and_then(|mut f| f.write_all(line.as_bytes()));
                }
            }
        }
    }

    fn set_tray(&self, tooltip: &str, status: &str, enabled: bool) {
        if let Some(t) = &self.tray
            && !tooltip.is_empty()
        {
            // Windows tooltips hold 127 UTF-16 units; tray-icon does not truncate safely
            let units: Vec<u16> = tooltip.encode_utf16().take(127).collect();
            let _ = t.set_tooltip(Some(String::from_utf16_lossy(&units)));
        }
        if let Some(m) = &self.menu {
            m.status.set_text(status);
            m.toggle
                .set_text(if enabled { "Pausar" } else { "Retomar" });
            m.check.set_enabled(enabled);
        }
    }

    fn ipc(&mut self, body: &str) {
        let m: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        if m["invoke"] == "state" {
            self.emit(HostEvent::InvokeState {
                id: m["id"].as_u64().unwrap_or(0),
            });
            return;
        }
        let ch = m["ch"].as_str().unwrap_or("");
        let args = m["args"].as_array().cloned().unwrap_or_default();
        match ch {
            "__bounds" => {
                if let (Some(web), Some(x), Some(y), Some(w), Some(h)) = (
                    &self.web,
                    args.first().and_then(Value::as_f64),
                    args.get(1).and_then(Value::as_f64),
                    args.get(2).and_then(Value::as_f64),
                    args.get(3).and_then(Value::as_f64),
                ) {
                    let sf = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
                    web.set_bounds(x, y, w, h, sf);
                }
            }
            "__leave" => {
                // Electron: the <webview> was inside .browser, so the cursor over the Tinder page kept it :hover
                let mut p = windows::Win32::Foundation::POINT::default();
                // SAFETY: plain query.
                let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p) };
                let (cx, cy) = self.window_rect();
                let sf = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
                let over =
                    self.web
                        .as_ref()
                        .and_then(|w| w.bounds(sf))
                        .is_some_and(|(x, y, w, h)| {
                            let (px, py) = ((p.x - cx) as f64, (p.y - cy) as f64);
                            px >= x && px < x + w && py >= y && py < y + h
                        });
                self.to_ui(&json!({ "ch": "__hover", "value": over }));
            }
            "__drag" => {
                if let Some(w) = &self.window {
                    // the user's click gives us foreground rights; an inactive window otherwise ignores the move request
                    if let Some(h) = self.hwnd() {
                        // SAFETY: our own window.
                        let _ = unsafe { SetForegroundWindow(h) };
                    }
                    let _ = w.drag_window();
                }
            }
            "__zoom" => {
                if let Some(w) = &self.window {
                    let _ = w;
                    self.toggle_zoom();
                }
            }
            "__resize" => {
                if let Some(w) = &self.window {
                    let dir = match args.first().and_then(Value::as_str).unwrap_or("") {
                        "n" => ResizeDirection::North,
                        "s" => ResizeDirection::South,
                        "e" => ResizeDirection::East,
                        "w" => ResizeDirection::West,
                        "ne" => ResizeDirection::NorthEast,
                        "nw" => ResizeDirection::NorthWest,
                        "se" => ResizeDirection::SouthEast,
                        _ => ResizeDirection::SouthWest,
                    };
                    let _ = w.drag_resize_window(dir);
                }
            }
            "toggle" | "check" | "win" | "set" => self.emit(HostEvent::Ipc {
                channel: ch.into(),
                args,
            }),
            _ => {}
        }
    }

    fn window_event(&mut self, e: WindowEvent, id: tao::window::WindowId) {
        let Some(w) = &self.window else { return };
        if id != w.id() {
            if let Some(web) = &mut self.web {
                web.popup_window_event(&e, id);
            }
            return;
        }
        match e {
            WindowEvent::CloseRequested => self.emit(HostEvent::WindowClose),
            WindowEvent::Resized(_) => {
                let m = self.zoomed();
                if m != self.maximized {
                    self.maximized = m;
                    self.to_ui(&json!({ "ch": "maximized", "value": m }));
                }
            }
            WindowEvent::Focused(_) => {
                // Electron focus/blur = window activation; moving focus into a child web view is not a blur
                // SAFETY: plain Win32 query.
                let active = self
                    .hwnd()
                    .is_some_and(|h| unsafe { GetForegroundWindow() } == h);
                if self.focused != Some(active) {
                    self.focused = Some(active);
                    self.to_ui(&json!({ "ch": "focused", "value": active }));
                }
            }
            _ => {}
        }
    }

    /// Electron isMaximized() on Windows = IsZoomed (tao's flag goes stale for this undecorated window).
    fn zoomed(&self) -> bool {
        // SAFETY: plain query on our own window.
        self.hwnd()
            .is_some_and(|h| unsafe { IsZoomed(h) }.as_bool())
    }

    fn toggle_zoom(&self) {
        self.show(if self.zoomed() {
            SW_RESTORE
        } else {
            SW_MAXIMIZE
        });
    }

    /// Screen position of the client area's top-left (what the user sees; the window rect also counts
    /// Windows' invisible resize borders, and tao's outer_position is stale while maximized).
    fn window_rect(&self) -> (i32, i32) {
        let Some(h) = self.hwnd() else { return (0, 0) };
        let mut p = windows::Win32::Foundation::POINT::default();
        // SAFETY: plain query on our own window.
        let _ = unsafe { windows::Win32::Graphics::Gdi::ClientToScreen(h, &mut p) };
        (p.x, p.y)
    }

    fn win_state(&self) -> WinState {
        let Some(h) = self.hwnd() else {
            return WinState::default();
        };
        // SAFETY: Win32 queries on our own window.
        unsafe {
            let minimized = IsIconic(h).as_bool();
            let visible = IsWindowVisible(h).as_bool() && !minimized; // Electron isVisible() is false while minimized
            let x = if minimized {
                let mut p = WINDOWPLACEMENT {
                    length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                    ..Default::default()
                };
                let _ = GetWindowPlacement(h, &mut p);
                p.rcNormalPosition.left
            } else {
                let mut r = Default::default();
                let _ = GetWindowRect(h, &mut r);
                r.left
            };
            WinState {
                minimized,
                visible,
                x,
            }
        }
    }

    fn show(&self, how: windows::Win32::UI::WindowsAndMessaging::SHOW_WINDOW_CMD) {
        if let Some(h) = self.hwnd() {
            // SAFETY: our own window.
            let _ = unsafe { ShowWindow(h, how) };
        }
    }

    /// main.js:302-307
    fn place_in_tray(&mut self) {
        let Some(w) = &self.window else { return };
        if self.win_state().x > -10000
            && let Ok(p) = w.outer_position()
        {
            self.tray_bounds = Some((p, w.outer_size()));
        }
        let _ = w.set_skip_taskbar(true);
        w.set_outer_position(PhysicalPosition::new(-32000, -32000));
        if !self.win_state().visible {
            self.show(SW_SHOWNOACTIVATE);
        }
    }

    /// main.js:308-313
    fn restore_from_tray(&mut self) {
        let Some(w) = &self.window else { return };
        let _ = w.set_skip_taskbar(false);
        let s = self.win_state();
        if s.minimized {
            self.show(SW_RESTORE);
        }
        if self.win_state().x <= -10000 {
            match self.tray_bounds {
                Some((p, size)) => {
                    w.set_inner_size(size);
                    w.set_outer_position(p);
                }
                None => center(w),
            }
        }
        self.show(SW_SHOW);
        if let Some(h) = self.hwnd() {
            // SAFETY: our own window.
            let _ = unsafe { SetForegroundWindow(h) };
        }
        w.set_focus();
    }

    fn teardown(&mut self) {
        self.tray = None;
        self.web = None;
        self.ui = None;
        self.window = None;
        if let Some(env) = self.env() {
            let _ = std::fs::remove_dir_all(&env.assets);
        }
    }
}

/// Electron centers a new window on the primary work area.
fn center(w: &Window) {
    if let Some(m) = w.current_monitor().or_else(|| w.primary_monitor()) {
        let (ms, mp, ws) = (m.size(), m.position(), w.outer_size());
        let x = mp.x + (ms.width as i32 - ws.width as i32) / 2;
        let y = mp.y + (ms.height as i32 - ws.height as i32) / 2;
        w.set_outer_position(PhysicalPosition::new(x, y));
    }
}
