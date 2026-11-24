//! Contract between the HOST (UI thread: windows, web views, tray, Windows APIs; `src/host/`) and the CORE
//! (logic thread: the port of main.js / engine.js / driver-web.js; `src/core/`). The core decides everything
//! main.js decides; the host only does what Electron did for main.js. Both sides depend only on this file.
//! SPEC references:
use std::{future::Future, pin::Pin, rc::Rc};

use serde_json::Value;

pub type Fut<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// The Tinder page as driver-web.js sees `wc` (S-06.3..S-06.7). Implemented by the host over CDP.
pub trait Page {
    /// `executeJavaScript(script)`: runs in the page main world, awaits a returned Promise, returns its JSON
    /// value. The 20 s race of driver-web.js:13-19 is the CORE's job (it knows the clock); the host just answers.
    fn eval(&self, script: &str) -> Fut<'_, Result<Value, String>>;
    /// `sendInputEvent` keyDown + keyUp; `key_code` is Electron's name: "Right" | "Left" | "Escape".
    fn key(&self, key_code: &'static str) -> Fut<'_, ()>;
    /// `loadURL(url)`: Ok when the navigation finished, Err when it failed or was aborted (callers swallow it).
    fn load(&self, url: &str) -> Fut<'_, Result<(), String>>;
    /// `getURL()`.
    fn url(&self) -> String;
    /// `isDestroyed()`.
    fn destroyed(&self) -> bool;
}

/// Window facts main.js reads (S-09.5).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WinState {
    pub minimized: bool,
    pub visible: bool,
    pub x: i32,
}

/// `app.getLoginItemSettings({args:['--hidden']})` (S-08.1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LoginItem {
    pub open_at_login: bool,
    pub executable_will_launch_at_login: bool,
}

/// main.js:331 initial placement of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// retry launch or `startMinimized`: `win.minimize()`.
    Minimized,
    /// `--hidden` without `startMinimized`: `toTray()`.
    Tray,
    /// opened by hand: `win.show()`.
    Shown,
}

/// Everything else main.js asks of Electron. The implementation lives on the logic thread and forwards to
/// the UI thread; calls are cheap and never block.
pub trait Host {
    /// main.js `createWindow()` + tray creation, after the startup checks (S-01.10 g-h). Nothing exists before.
    fn create_window(&self, placement: Placement);
    /// `win.webContents.send('state', snapshot)`.
    fn send_state(&self, snapshot: &Value);
    /// Reply to the renderer's `invoke('state')` with id `id`.
    fn reply_state(&self, id: u64, snapshot: &Value);
    /// `tray.setToolTip(tooltip)` + `buildTray()` with the current status text and `db.enabled`.
    fn set_tray(&self, tooltip: &str, status_text: &str, enabled: bool);
    /// `new Notification({ title: 'Spark', body, icon }).show()` (only when supported).
    fn notify(&self, body: &str);
    fn window(&self) -> WinState;
    fn minimize(&self);
    fn toggle_maximize(&self);
    /// main.js `fromTray()` (S-09.3).
    #[expect(clippy::wrong_self_convention, reason = "mirrors main.js fromTray()")]
    fn from_tray(&self);
    /// `app.quit()`: close everything and exit 0. The core has already done quitApp's bookkeeping.
    fn quit(&self);
    /// The Tinder page once attached (`did-attach-webview`), else None.
    fn page(&self) -> Option<Rc<dyn Page>>;
    /// `app.isPackaged` (release build installed by the installer).
    fn packaged(&self) -> bool;
    fn login_item(&self) -> LoginItem;
    /// `setLoginItemSettings({args:['--hidden'], openAtLogin, enabled})`; `enabled: None` = not passed.
    fn set_login_item(&self, open_at_login: bool, enabled: Option<bool>);
    /// Windows position for the page: Some((lat, lon, acc)) or None to clear the override.
    fn set_geolocation(&self, geo: Option<(f64, f64, f64)>);
}

/// Events the host delivers to the core, in order, on the logic thread.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEvent {
    /// Renderer `ipcRenderer.send(channel, ...args)`: toggle / check / win / set (JSON args as sent).
    Ipc {
        channel: String,
        args: Vec<Value>,
    },
    /// Renderer `ipcRenderer.invoke('state')`; answer with `Host::reply_state(id, ...)`.
    InvokeState {
        id: u64,
    },
    /// `did-attach-webview`: `Host::page()` is now Some.
    Attached,
    /// `did-start-loading` (also the moment to refresh the Windows position).
    StartLoading,
    /// `did-stop-loading` (main.js uses only the first one).
    StopLoading,
    /// `webRequest.onErrorOccurred` error text for tinder.com / *.tinder.com / *.gotinder.com URLs.
    NetError(String),
    /// Main-frame `did-fail-load` that is not an abort (Electron code != -3).
    FailLoad,
    /// `render-process-gone` with its reason.
    RenderGone(String),
    /// `second-instance` argv.
    SecondInstance(Vec<String>),
    /// `powerMonitor` resume.
    Resume,
    /// Window `session-end`.
    SessionEnd,
    /// Tray icon left click, or a tray menu item.
    TrayClick,
    TrayToggle,
    TrayCheck,
    TrayQuit,
    /// Native window close (Alt+F4, taskbar): main.js `win.on('close')` -> quitApp.
    WindowClose,
}
