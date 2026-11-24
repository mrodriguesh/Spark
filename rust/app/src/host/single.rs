//! Single instance, wire-compatible with Electron's `requestSingleInstanceLock()` (Chromium ProcessSingleton,
//! research/windows-integration.md §3): an Electron Spark and a Rust Spark on the same userData see each other.
use std::sync::{Mutex, OnceLock};

use tokio::sync::mpsc::UnboundedSender;

use crate::iface::HostEvent;

use windows::{
    Win32::{
        Foundation::{
            CloseHandle, GENERIC_WRITE, HANDLE, HWND, LPARAM, LRESULT, WAIT_FAILED, WPARAM,
        },
        Storage::FileSystem::{
            CREATE_ALWAYS, CreateFileW, FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_READ,
        },
        System::{
            DataExchange::COPYDATASTRUCT,
            Threading::{CreateMutexW, INFINITE, ReleaseMutex, WaitForSingleObject},
        },
        UI::WindowsAndMessaging::{
            AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CreateWindowExW, DefWindowProcW,
            FindWindowExW, GetWindowThreadProcessId, HWND_MESSAGE, MSGFLT_ALLOW, RegisterClassExW,
            SMTO_ABORTIFHUNG, SendMessageTimeoutW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_COPYDATA,
            WNDCLASSEXW,
        },
    },
    core::{HSTRING, PCWSTR, w},
};

const CLASS: PCWSTR = w!("Chrome_MessageWindow");

pub enum Lock {
    /// This process owns the profile; keep the guard alive for the process lifetime.
    First(Guard),
    /// Another instance (Electron or Rust) got our command line: exit 0.
    Notified,
    /// Locked but unreachable (`PROFILE_IN_USE` / `LOCK_ERROR`): exit 0 like main.js:46.
    Failed,
}

pub struct Guard {
    _lockfile: HANDLE,
}

static SINK: OnceLock<Mutex<Option<UnboundedSender<HostEvent>>>> = OnceLock::new();
static SHUTTING_DOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// argv of later launches arrive here (from the window procedure, on the thread that created the lock).
pub fn set_sink(tx: UnboundedSender<HostEvent>) {
    if let Ok(mut g) = SINK.get_or_init(|| Mutex::new(None)).lock() {
        *g = Some(tx);
    }
}

/// Chromium answers `!is_shutting_down()`; a launch during quit then exits too.
pub fn shutting_down() {
    SHUTTING_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn find(user_data: &HSTRING) -> Option<HWND> {
    // SAFETY: plain Win32 lookup with valid strings.
    unsafe { FindWindowExW(Some(HWND_MESSAGE), None, CLASS, user_data).ok() }
}

fn notify(hwnd: HWND) -> bool {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let cmdline = super::os::command_line();
    let payload: Vec<u16> = format!("START\0{cwd}\0{cmdline}\0")
        .encode_utf16()
        .collect();
    let cds = COPYDATASTRUCT {
        dwData: 0,
        cbData: (payload.len() * 2) as u32,
        lpData: payload.as_ptr() as *mut _,
    };
    let mut result = 0usize;
    // SAFETY: `cds` and `payload` outlive the synchronous SendMessageTimeoutW call.
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let _ = AllowSetForegroundWindow(pid);
        let sent = SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&cds as *const _ as isize),
            SMTO_ABORTIFHUNG,
            20_000,
            Some(&mut result),
        );
        sent.0 != 0 && result != 0
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        if SHUTTING_DOWN.load(std::sync::atomic::Ordering::SeqCst) {
            return LRESULT(0);
        }
        // SAFETY: WM_COPYDATA guarantees lparam points to a COPYDATASTRUCT valid during this call.
        let text = unsafe {
            let cds = &*(lparam.0 as *const COPYDATASTRUCT);
            let units =
                std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2);
            String::from_utf16_lossy(units)
        };
        let mut parts = text.split('\0');
        if parts.next() == Some("START") {
            let _cwd = parts.next();
            let argv = super::os::split_command_line(parts.next().unwrap_or(""));
            if let Some(Ok(g)) = SINK.get().map(|m| m.lock())
                && let Some(tx) = g.as_ref()
            {
                let _ = tx.send(HostEvent::SecondInstance(argv));
            }
            return LRESULT(1);
        }
        return LRESULT(0);
    }
    // SAFETY: default processing for every other message.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Must run on the UI thread (the message window lives there and is pumped by the tao loop).
pub fn acquire(user_data: &str) -> Lock {
    let title = HSTRING::from(user_data);
    if let Some(h) = find(&title) {
        return if notify(h) {
            Lock::Notified
        } else {
            Lock::Failed
        };
    }
    // SAFETY: Win32 calls with valid arguments; handles are closed or kept for the process lifetime.
    unsafe {
        let Ok(mutex) = CreateMutexW(None, false, w!("Local\\SparkProcessSingletonStartup")) else {
            return Lock::Failed;
        };
        if WaitForSingleObject(mutex, INFINITE) == WAIT_FAILED {
            let _ = CloseHandle(mutex);
            return Lock::Failed;
        }
        let release = || {
            let _ = ReleaseMutex(mutex);
            let _ = CloseHandle(mutex);
        };
        if let Some(h) = find(&title) {
            release();
            return if notify(h) {
                Lock::Notified
            } else {
                Lock::Failed
            };
        }
        let _ = std::fs::create_dir_all(user_data);
        let lockfile = CreateFileW(
            &HSTRING::from(format!("{user_data}\\lockfile")),
            GENERIC_WRITE.0,
            FILE_SHARE_READ,
            None,
            CREATE_ALWAYS,
            FILE_FLAG_DELETE_ON_CLOSE,
            None,
        );
        let Ok(lockfile) = lockfile else {
            release();
            return Lock::Failed;
        };
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            &title,
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        );
        release();
        match hwnd {
            Ok(h) => {
                let _ = ChangeWindowMessageFilterEx(h, WM_COPYDATA, MSGFLT_ALLOW, None);
                Lock::First(Guard {
                    _lockfile: lockfile,
                })
            }
            Err(_) => Lock::Failed,
        }
    }
}
