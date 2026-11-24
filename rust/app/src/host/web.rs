//! The two web views: the UI (index.html byte-identical + bridge) and the Tinder page in the
//! `tinder` profile with Electron's webContents behavior rebuilt on raw WebView2 + CDP. Mechanisms measured
//! in
use std::{borrow::Cow, cell::RefCell, collections::HashMap, rc::Rc};

use serde_json::{Value, json};
use tao::{
    dpi::{LogicalPosition, LogicalSize},
    event::WindowEvent,
    event_loop::{EventLoopProxy, EventLoopWindowTarget},
    window::{Icon, Window, WindowBuilder, WindowId},
};
use tokio::sync::{mpsc::UnboundedSender, oneshot};
use webview2_com::{
    CallDevToolsProtocolMethodCompletedHandler, DevToolsProtocolEventReceivedEventHandler,
    Microsoft::Web::WebView2::Win32::*, NavigationCompletedEventHandler,
    NavigationStartingEventHandler, PermissionRequestedEventHandler, ProcessFailedEventHandler,
    WindowCloseRequestedEventHandler, take_pwstr,
};
use windows::core::{BOOL, HSTRING, PWSTR};
use wry::{
    NewWindowFeatures, NewWindowResponse, Rect, WebContext, WebView, WebViewBuilder,
    WebViewBuilderExtWindows, WebViewExtWindows,
    http::{Request, Response},
};

use super::{Cmd, Env, INDEX_HTML};
use crate::iface::HostEvent;

const BG: (u8, u8, u8, u8) = (0x07, 0x02, 0x0D, 255); // main.js:315
/// wry's defaults (no Edge mini menu / PDF UI / SmartScreen) + Electron's occlusion switch (main.js:44)
const ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --disable-backgrounding-occluded-windows --disable-renderer-backgrounding --disable-background-timer-throttling";
/// WebView2 COREWEBVIEW2_WEB_ERROR_STATUS_OPERATION_CANCELED: Electron's -3 (aborted) is not a failure
const OPERATION_CANCELED: i32 = 14;

fn host_of(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let auth = rest.split(['/', '?', '#']).next()?;
    let host = auth.rsplit('@').next()?.split(':').next()?;
    Some(host.to_ascii_lowercase())
}

/// main.js:33-38, ported and recorded in the main-pure unit (WHATWG URL parsing).
pub fn allowed(url: &str) -> bool {
    spark_main_pure::port::allowed_str(url)
}

/// main.js:360 filter: https://tinder.com/*, https://*.tinder.com/*, https://*.gotinder.com/*
fn tinder_url(url: &str) -> bool {
    host_of(url).is_some_and(|h| {
        h == "tinder.com"
            || h.ends_with(".tinder.com")
            || h == "gotinder.com"
            || h.ends_with(".gotinder.com")
    })
}

pub enum WebCmd {
    MakeSpare,
    ClosePopup(isize),
    Reapply,
}

struct Popup {
    window: Window,
    _view: WebView,
    id: isize,
}

/// a `loadURL` waiting for its navigation id, then for that navigation to finish
type PendingLoad = (Option<u64>, oneshot::Sender<Result<(), String>>);

#[derive(Default)]
struct Shared {
    pending: Vec<PendingLoad>,
    urls: HashMap<String, String>,
    geo: Option<(f64, f64, f64)>,
    popups: Vec<Popup>,
    spare: Option<Window>,
}

pub struct TinderWeb {
    view: WebView,
    core: ICoreWebView2,
    shared: Rc<RefCell<Shared>>,
    icon: Option<Icon>,
    _ctx: Box<WebContext>,
}

fn browser_args() -> String {
    let mut a = String::from(ARGS);
    if cfg!(feature = "test-hooks") {
        a.push_str(" --proxy-server=http://127.0.0.1:9"); // tests never reach the internet (spike 1 incident)
        if let Some(port) = super::test_setting("SPARK_TEST_CDP_PORT") {
            a.push_str(&format!(" --remote-debugging-port={port}"));
        }
    }
    a
}

fn cdp(
    core: &ICoreWebView2,
    method: &str,
    params: Value,
    done: impl FnOnce(Result<Value, String>) + 'static,
) {
    let done = Rc::new(RefCell::new(Some(done)));
    let d2 = done.clone();
    let handler = CallDevToolsProtocolMethodCompletedHandler::create(Box::new(move |hr, json| {
        if let Some(f) = d2.borrow_mut().take() {
            f(hr.map_err(|e| e.message())
                .and_then(|_| serde_json::from_str(&json).map_err(|e| e.to_string())));
        }
        Ok(())
    }));
    // SAFETY: UI thread that owns `core`; arguments outlive the call.
    let r = unsafe {
        core.CallDevToolsProtocolMethod(
            &HSTRING::from(method),
            &HSTRING::from(params.to_string()),
            &handler,
        )
    };
    if let Err(e) = r
        && let Some(f) = done.borrow_mut().take()
    {
        f(Err(e.message()));
    }
}

fn subscribe(
    core: &ICoreWebView2,
    event: &str,
    mut f: impl FnMut(Value) + 'static,
) -> windows::core::Result<()> {
    // SAFETY: UI thread; WebView2 owns the handler.
    unsafe {
        let recv = core.GetDevToolsProtocolEventReceiver(&HSTRING::from(event))?;
        let mut token = 0i64;
        recv.add_DevToolsProtocolEventReceived(
            &DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut p = PWSTR::null();
                    args.ParameterObjectAsJson(&mut p)?;
                    f(serde_json::from_str(&take_pwstr(p)).unwrap_or(Value::Null));
                }
                Ok(())
            })),
            &mut token,
        )
    }
}

fn apply_geo(core: &ICoreWebView2, geo: Option<(f64, f64, f64)>) {
    match geo {
        Some((lat, lon, acc)) => cdp(
            core,
            "Emulation.setGeolocationOverride",
            json!({ "latitude": lat, "longitude": lon, "accuracy": acc }),
            |_| {},
        ),
        None => cdp(
            core,
            "Emulation.clearGeolocationOverride",
            json!({}),
            |_| {},
        ),
    }
}

/// test-hooks: answer https://tinder.com/* with a local page (path in SPARK_TEST_FAKE_PAGE).
#[cfg(feature = "test-hooks")]
fn fake_page(core: &ICoreWebView2, env: &ICoreWebView2Environment) {
    use webview2_com::WebResourceRequestedEventHandler;
    use windows::Win32::UI::Shell::SHCreateMemStream;
    let Some(path) = super::test_setting("SPARK_TEST_FAKE_PAGE") else {
        return;
    };
    let Ok(page) = std::fs::read(path) else {
        return;
    };
    let env = env.clone();
    // SAFETY: UI thread; WebView2 owns the handler.
    unsafe {
        let mut token = 0i64;
        let _ = core.AddWebResourceRequestedFilter(
            &HSTRING::from("https://tinder.com/*"),
            COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL,
        );
        let _ = core.add_WebResourceRequested(
            &webview2_com::WebResourceRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let stream = SHCreateMemStream(Some(&page));
                let resp = env.CreateWebResourceResponse(
                    stream.as_ref(),
                    200,
                    &HSTRING::from("OK"),
                    &HSTRING::from("Content-Type: text/html; charset=utf-8"),
                )?;
                args.SetResponse(&resp)
            })),
            &mut token,
        );
        let _: Option<WebResourceRequestedEventHandler> = None;
    }
}
#[cfg(not(feature = "test-hooks"))]
fn fake_page(_: &ICoreWebView2, _: &ICoreWebView2Environment) {}

/// main.js:368-375 for every Tinder-profile view: allowed popups open 520x700 in the same profile.
fn new_window_handler(
    shared: Rc<RefCell<Shared>>,
    proxy: EventLoopProxy<Cmd>,
    icon: Option<Icon>,
) -> impl Fn(String, NewWindowFeatures) -> NewWindowResponse + 'static {
    move |url, features| {
        if !(url.is_empty() || url == "about:blank" || allowed(&url)) {
            return NewWindowResponse::Deny;
        }
        let Some(win) = shared.borrow_mut().spare.take() else {
            return NewWindowResponse::Deny;
        };
        let built = WebViewBuilder::new()
            .with_environment(features.opener.environment.clone())
            .with_profile_name("tinder")
            .with_background_color(BG)
            .with_navigation_handler(|u: String| allowed(&u))
            .with_new_window_req_handler(new_window_handler(
                shared.clone(),
                proxy.clone(),
                icon.clone(),
            ))
            .build(&win);
        let Ok(view) = built else {
            return NewWindowResponse::Deny;
        };
        let core = view.webview();
        fake_page(&core, &features.opener.environment);
        let id = win.hwnd();
        let p = proxy.clone();
        let mut token = 0i64;
        // SAFETY: UI thread; WebView2 owns the handler.
        let _ = unsafe {
            core.add_WindowCloseRequested(
                &WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
                    let _ = p.send_event(Cmd::Web(WebCmd::ClosePopup(id)));
                    Ok(())
                })),
                &mut token,
            )
        };
        win.set_visible(true);
        shared.borrow_mut().popups.push(Popup {
            window: win,
            _view: view,
            id,
        });
        let _ = proxy.send_event(Cmd::Web(WebCmd::MakeSpare));
        NewWindowResponse::Create { webview: core }
    }
}

use tao::platform::windows::WindowExtWindows as _;

/// Builds the UI view (auto-resized with the window) and the Tinder child view (follows the placeholder).
pub fn build(
    window: &Window,
    env: &Env,
    proxy: EventLoopProxy<Cmd>,
    events: Option<UnboundedSender<HostEvent>>,
    icon: Option<Icon>,
) -> wry::Result<(WebView, TinderWeb)> {
    let mut ctx = Box::new(WebContext::new(Some(env.user_data.join("WebView2"))));
    let args = browser_args();
    let icon_png = super::ICON_PNG;
    let ipc_proxy = proxy.clone();
    let ui = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_additional_browser_args(&args)
        .with_background_color(BG)
        .with_initialization_script(include_str!("bridge.js"))
        .with_custom_protocol("spark".into(), move |_, req: Request<Vec<u8>>| {
            let (body, mime): (&'static [u8], &str) = match req.uri().path() {
                "/" | "/index.html" => (INDEX_HTML, "text/html; charset=utf-8"),
                "/build/icon.png" => (icon_png, "image/png"),
                _ => (b"", "text/plain"),
            };
            Response::builder()
                .header("Content-Type", mime)
                .body(Cow::Borrowed(body))
                .unwrap_or_else(|_| Response::new(Cow::Borrowed(&b""[..])))
        })
        .with_navigation_handler(|url| url.starts_with("http://spark.localhost/"))
        .with_ipc_handler(move |req: Request<String>| {
            let _ = ipc_proxy.send_event(Cmd::Ipc(req.into_body()));
        })
        .with_url("http://spark.localhost/index.html")
        .build(window)?;

    let shared = Rc::new(RefCell::new(Shared::default()));
    let view = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_additional_browser_args(&args)
        .with_profile_name("tinder")
        .with_background_color(BG)
        .with_focused(false)
        .with_bounds(Rect {
            position: LogicalPosition::new(399.0, 19.0).into(),
            size: LogicalSize::new(902.0, 818.0).into(),
        })
        .with_new_window_req_handler(new_window_handler(
            shared.clone(),
            proxy.clone(),
            icon.clone(),
        ))
        .build_as_child(window)?;
    let core = view.webview();
    let wenv = view.environment();
    fake_page(&core, &wenv);
    hook(&core, &shared, proxy.clone(), events)
        .map_err(|e| wry::Error::WebView2Error(webview2_com::Error::WindowsError(e)))?;
    let _ = proxy.send_event(Cmd::Web(WebCmd::MakeSpare));
    Ok((
        ui,
        TinderWeb {
            view,
            core,
            shared,
            icon,
            _ctx: ctx,
        },
    ))
}

/// main.js:339-365 on the raw ICoreWebView2.
fn hook(
    core: &ICoreWebView2,
    shared: &Rc<RefCell<Shared>>,
    proxy: EventLoopProxy<Cmd>,
    events: Option<UnboundedSender<HostEvent>>,
) -> windows::core::Result<()> {
    let emit = move |e: HostEvent| {
        if let Some(tx) = &events {
            let _ = tx.send(e);
        }
    };
    // SAFETY: all calls on the UI thread that owns `core`; WebView2 owns the handlers.
    unsafe {
        let settings = core.Settings()?;
        settings.SetIsWebMessageEnabled(false)?; // nothing of ours visible to the page (S-07.7)
        settings.SetAreHostObjectsAllowed(false)?;
        let mut token = 0i64;
        let (s, e, c) = (shared.clone(), emit.clone(), core.clone());
        core.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut p = PWSTR::null();
                args.Uri(&mut p)?;
                let uri = take_pwstr(p);
                let mut redirected = BOOL::default();
                args.IsRedirected(&mut redirected)?;
                let mut id = 0u64;
                args.NavigationId(&mut id)?;
                if !allowed(&uri) {
                    args.SetCancel(true)?; // will-navigate / will-redirect preventDefault
                }
                let mut sh = s.borrow_mut();
                for entry in sh.pending.iter_mut().filter(|(n, _)| n.is_none()) {
                    entry.0 = Some(id);
                }
                if allowed(&uri) && !redirected.as_bool() {
                    apply_geo(&c, sh.geo);
                    drop(sh);
                    e(HostEvent::StartLoading);
                }
                Ok(())
            })),
            &mut token,
        )?;
        let (s, e) = (shared.clone(), emit.clone());
        core.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut ok = BOOL::default();
                args.IsSuccess(&mut ok)?;
                let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                args.WebErrorStatus(&mut status)?;
                let mut id = 0u64;
                args.NavigationId(&mut id)?;
                if !ok.as_bool() && status.0 != OPERATION_CANCELED {
                    e(HostEvent::FailLoad);
                }
                e(HostEvent::StopLoading);
                let done: Vec<_> = {
                    let mut sh = s.borrow_mut();
                    let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut sh.pending)
                        .into_iter()
                        .partition(|(n, _)| *n == Some(id));
                    sh.pending = rest;
                    mine
                };
                for (_, reply) in done {
                    let _ = reply.send(if ok.as_bool() {
                        Ok(())
                    } else {
                        Err(format!("WebErrorStatus {}", status.0))
                    });
                }
                Ok(())
            })),
            &mut token,
        )?;
        let (e, p) = (emit.clone(), proxy.clone());
        core.add_ProcessFailed(
            &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                args.ProcessFailedKind(&mut kind)?;
                let mut reason = COREWEBVIEW2_PROCESS_FAILED_REASON::default();
                if let Ok(a2) =
                    windows_core::Interface::cast::<ICoreWebView2ProcessFailedEventArgs2>(&args)
                {
                    let _ = a2.Reason(&mut reason);
                }
                if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED {
                    let _ = p.send_event(Cmd::Web(WebCmd::Reapply));
                    return Ok(());
                }
                if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
                    || kind == COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED
                {
                    // Electron render-process-gone reasons
                    let r = match reason {
                        COREWEBVIEW2_PROCESS_FAILED_REASON_CRASHED => "crashed",
                        COREWEBVIEW2_PROCESS_FAILED_REASON_TERMINATED => "killed",
                        COREWEBVIEW2_PROCESS_FAILED_REASON_OUT_OF_MEMORY => "oom",
                        COREWEBVIEW2_PROCESS_FAILED_REASON_LAUNCH_FAILED => "launch-failed",
                        _ => "abnormal-exit",
                    };
                    e(HostEvent::RenderGone(r.into()));
                }
                Ok(())
            })),
            &mut token,
        )?;
        core.add_PermissionRequested(
            &PermissionRequestedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_PERMISSION_KIND::default();
                args.PermissionKind(&mut kind)?;
                let mut p = PWSTR::null();
                args.Uri(&mut p)?;
                if kind == COREWEBVIEW2_PERMISSION_KIND_GEOLOCATION && allowed(&take_pwstr(p)) {
                    args.SetState(COREWEBVIEW2_PERMISSION_STATE_ALLOW)?;
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    // webRequest.onErrorOccurred (main.js:360-361): CDP network failures joined to their URL
    let s = shared.clone();
    subscribe(core, "Network.requestWillBeSent", move |v| {
        if let (Some(id), Some(url)) = (v["requestId"].as_str(), v["request"]["url"].as_str()) {
            s.borrow_mut().urls.insert(id.into(), url.into());
        }
    })?;
    let s = shared.clone();
    subscribe(core, "Network.loadingFinished", move |v| {
        if let Some(id) = v["requestId"].as_str() {
            s.borrow_mut().urls.remove(id);
        }
    })?;
    let s = shared.clone();
    subscribe(core, "Network.loadingFailed", move |v| {
        let url = v["requestId"]
            .as_str()
            .and_then(|id| s.borrow_mut().urls.remove(id));
        if let (Some(url), Some(err)) = (url, v["errorText"].as_str())
            && tinder_url(&url)
        {
            emit(HostEvent::NetError(err.into()));
        }
    })?;
    cdp(core, "Network.enable", json!({}), |_| {});
    Ok(())
}

/// Inner radius of `.browser` in index.html: `--r` (20px) minus its 1px border and 2px padding.
const INNER_RADIUS: f64 = 17.0;

/// The Tinder view is a native child window (a rectangle); clip it to the neon frame's rounded inner edge.
fn round_corners(view: &WebView, w: f64, h: f64, r: f64) {
    use windows::Win32::{
        Graphics::Gdi::{CreateRoundRectRgn, SetWindowRgn},
        UI::WindowsAndMessaging::IsWindow,
    };
    let mut hwnd = windows::Win32::Foundation::HWND::default();
    // SAFETY: plain Win32 calls on the view's own container window; SetWindowRgn takes ownership of the region.
    unsafe {
        if view.controller().ParentWindow(&mut hwnd).is_err() || !IsWindow(Some(hwnd)).as_bool() {
            return;
        }
        let (w, h, d) = (w.round() as i32, h.round() as i32, (r * 2.0).round() as i32);
        let rgn = CreateRoundRectRgn(0, 0, w + 1, h + 1, d, d); // right/bottom are exclusive
        SetWindowRgn(hwnd, Some(rgn), true);
    }
}

impl TinderWeb {
    pub fn set_bounds(&self, x: f64, y: f64, w: f64, h: f64, scale: f64) {
        let _ = self.view.set_bounds(Rect {
            position: LogicalPosition::new(x, y).into(),
            size: LogicalSize::new(w, h).into(),
        });
        round_corners(&self.view, w * scale, h * scale, INNER_RADIUS * scale);
    }

    /// Rect of the Tinder view inside the window, in physical pixels.
    pub fn bounds(&self, scale: f64) -> Option<(f64, f64, f64, f64)> {
        let b = self.view.bounds().ok()?;
        let (p, z) = (
            b.position.to_physical::<f64>(scale),
            b.size.to_physical::<f64>(scale),
        );
        Some((p.x, p.y, z.width, z.height))
    }

    pub fn set_geo(&mut self, geo: Option<(f64, f64, f64)>) {
        self.shared.borrow_mut().geo = geo;
        apply_geo(&self.core, geo);
    }

    pub fn eval(&self, script: &str, reply: oneshot::Sender<Result<Value, String>>) {
        cdp(
            &self.core,
            "Runtime.evaluate",
            json!({ "expression": script, "awaitPromise": true, "returnByValue": true }),
            move |r| {
                let out = r.and_then(|v| match v.get("exceptionDetails") {
                    Some(ex) => Err(ex["exception"]["description"]
                        .as_str()
                        .or(ex["text"].as_str())
                        .unwrap_or("Error")
                        .to_string()),
                    None => Ok(v["result"].get("value").cloned().unwrap_or(Value::Null)),
                });
                let _ = reply.send(out);
            },
        );
    }

    /// Electron `sendInputEvent({type:'keyDown'|'keyUp', keyCode})`.
    pub fn key(&self, key_code: &str) {
        let (key, vk) = match key_code {
            "Right" => ("ArrowRight", 39),
            "Left" => ("ArrowLeft", 37),
            _ => ("Escape", 27),
        };
        for t in ["rawKeyDown", "keyUp"] {
            cdp(
                &self.core,
                "Input.dispatchKeyEvent",
                json!({ "type": t, "key": key, "code": key, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk }),
                |_| {},
            );
        }
    }

    pub fn load(&mut self, url: &str, reply: oneshot::Sender<Result<(), String>>) {
        // SAFETY: UI thread that owns `core`.
        match unsafe { self.core.Navigate(&HSTRING::from(url)) } {
            Ok(()) => self.shared.borrow_mut().pending.push((None, reply)),
            Err(e) => {
                let _ = reply.send(Err(e.message()));
            }
        }
    }

    pub fn url(&self) -> String {
        let mut p = PWSTR::null();
        // SAFETY: UI thread; the string is taken and freed by take_pwstr.
        match unsafe { self.core.Source(&mut p) } {
            Ok(()) => take_pwstr(p),
            Err(_) => String::new(),
        }
    }

    pub fn browser_pid(&self) -> u32 {
        let mut pid = 0u32;
        // SAFETY: UI thread; plain getter.
        let _ = unsafe { self.core.BrowserProcessId(&mut pid) };
        pid
    }

    pub fn on_cmd(&mut self, c: WebCmd, target: &EventLoopWindowTarget<Cmd>) {
        match c {
            WebCmd::MakeSpare => {
                let w = WindowBuilder::new()
                    .with_title("Spark")
                    .with_visible(false)
                    .with_window_icon(self.icon.clone())
                    .with_inner_size(LogicalSize::new(520.0, 700.0))
                    .build(target)
                    .ok();
                self.shared.borrow_mut().spare = w;
            }
            WebCmd::ClosePopup(id) => self.shared.borrow_mut().popups.retain(|p| p.id != id),
            WebCmd::Reapply => {}
        }
    }

    pub fn popup_window_event(&mut self, e: &WindowEvent, id: WindowId) {
        if let WindowEvent::CloseRequested = e {
            self.shared
                .borrow_mut()
                .popups
                .retain(|p| p.window.id() != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{allowed, tinder_url};
    #[test]
    fn allowlist_like_main_js() {
        assert!(allowed("https://tinder.com/app/recs"));
        assert!(allowed("https://accounts.google.com/x"));
        assert!(!allowed("http://tinder.com/"));
        assert!(!allowed("https://evil-tinder.com/"));
        assert!(!allowed("https://example.com/"));
        assert!(!allowed("not a url"));
        assert!(tinder_url("https://api.gotinder.com/v2"));
        assert!(tinder_url("https://gotinder.com/"));
        assert!(!tinder_url("https://google.com/"));
    }
}
