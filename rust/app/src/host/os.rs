//! Windows integration that Electron did for main.js: AUMID, command line, login item, toasts, power/session.
use tokio::sync::mpsc::UnboundedSender as Sender;

use windows::{
    Data::Xml::Dom::XmlDocument,
    UI::Notifications::{ToastNotification, ToastNotificationManager},
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, WPARAM},
        System::Environment::GetCommandLineW,
        UI::{
            Shell::{CommandLineToArgvW, SetCurrentProcessExplicitAppUserModelID},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND,
                PBT_APMSUSPEND, RegisterClassExW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_POWERBROADCAST,
                WM_QUERYENDSESSION, WNDCLASSEXW,
            },
        },
    },
    core::{HSTRING, PCWSTR, w},
};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, RegType},
    types::FromRegValue,
};

use crate::iface::{HostEvent, LoginItem};

pub const AUMID: &str = "app.spark.match";

/// main.js:42 `app.setAppUserModelId`; before any UI (toasts and taskbar identity).
pub fn set_aumid() {
    // SAFETY: valid static string.
    let _ = unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(AUMID)) };
}

pub fn command_line() -> String {
    // SAFETY: GetCommandLineW returns a process-lifetime string.
    unsafe { GetCommandLineW().to_string().unwrap_or_default() }
}

/// Chromium `CommandLine::FromString` splits with `CommandLineToArgvW`.
pub fn split_command_line(cmd: &str) -> Vec<String> {
    if cmd.is_empty() {
        return Vec::new();
    }
    let mut n = 0;
    // SAFETY: the returned array is valid until LocalFree; we copy the strings out.
    unsafe {
        let p = CommandLineToArgvW(&HSTRING::from(cmd), &mut n);
        if p.is_null() {
            return Vec::new();
        }
        let out = (0..n as usize)
            .map(|i| (*p.add(i)).to_string().unwrap_or_default())
            .collect();
        let _ =
            windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(p as _)));
        out
    }
}

// ---------- login item (research/windows-integration.md §2, Electron browser_win.cc) ----------
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// Chromium `AddQuoteForArg`.
fn add_quote_for_arg(a: &str) -> String {
    if !a.contains([' ', '\\', '"']) {
        return a.into();
    }
    let c: Vec<char> = a.chars().collect();
    let (mut o, mut i) = (String::from('"'), 0);
    while i < c.len() {
        if c[i] == '\\' {
            let s = i;
            while i < c.len() && c[i] == '\\' {
                i += 1;
            }
            let n = if i == c.len() || c[i] == '"' {
                (i - s) * 2
            } else {
                i - s
            };
            o.extend(std::iter::repeat_n('\\', n));
            continue;
        }
        if c[i] == '"' {
            o.push_str("\\\"");
        } else {
            o.push(c[i]);
        }
        i += 1;
    }
    o.push('"');
    o
}

fn run_command(exe: &str) -> String {
    format!(
        "{} {}",
        add_quote_for_arg(exe.trim_matches('"')),
        add_quote_for_arg("--hidden")
    )
}

fn approved(hive: &RegKey, name: &str) -> bool {
    let Ok(key) = hive.open_subkey(APPROVED) else {
        return true;
    };
    match key.get_raw_value(name) {
        Ok(v) if v.vtype == RegType::REG_BINARY && v.bytes.len() <= 24 => {
            let prefix: Vec<u8> = v.bytes.iter().copied().take_while(|&b| b != 0).collect();
            prefix.is_empty() || prefix == [2]
        }
        _ => true,
    }
}

pub fn login_item(exe: &str) -> LoginItem {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let open_at_login = hkcu
        .open_subkey(RUN)
        .and_then(|k| k.get_value::<String, _>(AUMID))
        .map(|v| v == run_command(exe))
        .unwrap_or(false);
    let want = split_command_line(exe.trim_matches('"'))
        .into_iter()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let mut will_launch = false;
    for hive in [
        RegKey::predef(HKEY_CURRENT_USER),
        RegKey::predef(HKEY_LOCAL_MACHINE),
    ] {
        let Ok(run) = hive.open_subkey(RUN) else {
            continue;
        };
        for (name, value) in run.enum_values().flatten() {
            let Ok(cmd) = String::from_reg_value(&value) else {
                continue;
            };
            let prog = split_command_line(&cmd)
                .into_iter()
                .next()
                .unwrap_or_default()
                .to_lowercase();
            if prog == want && approved(&hive, &name) {
                will_launch = true;
            }
        }
    }
    LoginItem {
        open_at_login,
        executable_will_launch_at_login: will_launch,
    }
}

pub fn set_login_item(exe: &str, open_at_login: bool, enabled: Option<bool>) {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok((run, _)) = hkcu.create_subkey_with_flags(RUN, KEY_ALL_ACCESS) else {
        return;
    };
    let Ok((ok, _)) = hkcu.create_subkey_with_flags(APPROVED, KEY_ALL_ACCESS) else {
        return;
    };
    if open_at_login {
        let _ = run.set_value(AUMID, &run_command(exe));
        if enabled.unwrap_or(true) {
            let _ = ok.delete_value(AUMID);
        } else {
            let _ = ok.set_raw_value(
                AUMID,
                &winreg::RegValue {
                    vtype: RegType::REG_BINARY,
                    bytes: vec![3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0].into(),
                },
            );
        }
    } else {
        let _ = ok.delete_value(AUMID);
        let _ = run.delete_value(AUMID);
    }
}

// ---------- toasts (Electron windows_toast_notification.cc) ----------
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// `new Notification({ title: 'Spark', body, icon }).show()`. Errors are swallowed like main.js.
pub fn toast(body: &str, icon_png: &std::path::Path) {
    let xml = format!(
        r#"<toast><visual><binding template="ToastImageAndText02"><text id="1">Spark</text><text id="2">{}</text><image id="1" placement="appLogoOverride" hint-crop="none" src="{}"/></binding></visual></toast>"#,
        xml_escape(body),
        xml_escape(&icon_png.display().to_string())
    );
    let show = || -> windows::core::Result<()> {
        let doc = XmlDocument::new()?;
        doc.LoadXml(&HSTRING::from(xml.as_str()))?;
        let toast = ToastNotification::CreateToastNotification(&doc)?;
        toast.SetGroup(&HSTRING::from("Notifications"))?;
        toast.SetTag(&HSTRING::from(format!(
            "{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_micros())
                .unwrap_or(0)
                & 0xffff_ffff
        )))?;
        ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?.Show(&toast)
    };
    let shown = show();
    #[cfg(feature = "test-hooks")]
    if let Ok(mut g) = TOASTS.lock() {
        g.push(serde_json::json!({ "xml": xml, "shown": shown.is_ok(), "error": shown.as_ref().err().map(|e| e.message()) }));
    }
    let _ = shown;
}

/// test-hooks: every toast the app asked Windows to show (control port `toasts`).
#[cfg(feature = "test-hooks")]
pub static TOASTS: std::sync::Mutex<Vec<serde_json::Value>> = std::sync::Mutex::new(Vec::new());

// ---------- power and session (Electron powerMonitor 'resume', BrowserWindow 'session-end') ----------
static POWER: std::sync::OnceLock<std::sync::Mutex<(Sender<HostEvent>, bool)>> =
    std::sync::OnceLock::new();

extern "system" fn power_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if let Some(Ok(mut g)) = POWER.get().map(|m| m.lock()) {
        if msg == WM_POWERBROADCAST {
            match wparam.0 as u32 {
                PBT_APMSUSPEND => g.1 = true,
                // Electron emits 'resume' only after it saw the suspend
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND if g.1 => {
                    g.1 = false;
                    let _ = g.0.send(HostEvent::Resume);
                }
                _ => {}
            }
        } else if msg == WM_QUERYENDSESSION {
            let _ = g.0.send(HostEvent::SessionEnd);
            drop(g);
            std::thread::sleep(std::time::Duration::from_millis(500)); // let the logic thread save (main.js:328)
            return LRESULT(1);
        }
    }
    // SAFETY: default processing.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// A hidden top-level window (message-only windows get no broadcasts). UI thread.
pub fn power_window(tx: Sender<HostEvent>) {
    let _ = POWER.set(std::sync::Mutex::new((tx, false)));
    const CLASS: PCWSTR = w!("SparkPowerWindow");
    // SAFETY: registering a class and creating a never-shown window with valid arguments.
    unsafe {
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(power_proc),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
        let _ = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("Spark"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        );
    }
}

/// test-hooks: the window as the user sees it (both web views), like Electron `capturePage()` -> PNG.
#[cfg(feature = "test-hooks")]
pub fn capture(hwnd: HWND, path: &std::path::Path) -> Result<(), String> {
    use windows::Win32::{
        Foundation::RECT,
        Graphics::Gdi::{
            BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleBitmap, CreateCompatibleDC,
            DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SelectObject,
        },
        Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow},
        UI::WindowsAndMessaging::GetClientRect,
    };
    // SAFETY: GDI objects are created, used and released within this block on our own window.
    let (w, h, mut buf) = unsafe {
        let mut r = RECT::default();
        GetClientRect(hwnd, &mut r).map_err(|e| e.to_string())?;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let hdc = GetDC(Some(hwnd));
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, bmp.into());
        let _ = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(3)); // PW_CLIENTONLY | PW_RENDERFULLCONTENT
        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        GetDIBits(
            mem,
            bmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as _),
            &mut bi,
            DIB_RGB_COLORS,
        );
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(Some(hwnd), hdc);
        (w as u32, h as u32, buf)
    };
    for px in buf.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
        px[3] = 255;
    }
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut wr| wr.write_image_data(&buf))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn quoting_matches_electron() {
        assert_eq!(
            super::run_command(r"C:\Users\User\AppData\Local\Programs\spark\Spark.exe"),
            r#""C:\Users\User\AppData\Local\Programs\spark\Spark.exe" --hidden"#
        );
        assert_eq!(super::add_quote_for_arg(r#"a\b"c\"#), r#""a\b\"c\\""#);
        assert_eq!(super::add_quote_for_arg("plain"), "plain");
    }
}
