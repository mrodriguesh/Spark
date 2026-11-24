//!the Tinder login survives the Electron -> Rust upgrade. On the first start over a userData that has
//! Electron's `persist:tinder` partition, its login stores are copied into the WebView2 `tinder` profile before
//! any Tinder view exists. The Electron partition is
//! only read, so a rollback to Electron finds it intact. Cookies need no conversion: Electron's cookie-encryption
//! fuse is off, and WebView2 loads plaintext rows as they are (proven by `rust/tests-js/migration.js`).
use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use webview2_com::{
    BrowserProcessExitedEventHandler, CoreWebView2EnvironmentOptions,
    CreateCoreWebView2ControllerCompletedHandler, CreateCoreWebView2EnvironmentCompletedHandler,
    Microsoft::Web::WebView2::Win32::*, take_pwstr,
};
use windows::{
    Win32::{
        Foundation::{E_POINTER, HWND},
        UI::WindowsAndMessaging::{
            DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
            TranslateMessage,
        },
    },
    core::{HSTRING, Interface, PCWSTR, PWSTR},
};

/// Paths relative to the partition and to the profile. Each target becomes exactly its source, absence
/// included (a leftover `Cookies-journal` next to a replaced `Cookies` would corrupt it).
const STORES: &[&str] = &[
    "Network\\Cookies",
    "Network\\Cookies-journal",
    "Local Storage\\leveldb",
    "IndexedDB\\https_tinder.com_0.indexeddb.leveldb",
    "IndexedDB\\https_tinder.com_0.indexeddb.blob",
];
/// In `<userData>\WebView2`; written only after every store is in place.
const MARKER: &str = "spark-migrated";
const STAGING: &str = "spark-migration";

/// Runs before `web::build`, on the UI thread, while no WebView2 of this process exists.
pub fn run(user_data: &Path, hwnd: HWND, icon: &Path) {
    let src = user_data.join("Partitions").join("tinder");
    let udf = user_data.join("WebView2");
    if !src.is_dir() || udf.join(MARKER).exists() {
        return;
    }
    if let Err(e) = profile_path(&udf, hwnd).and_then(|profile| copy(&src, &udf, &profile)) {
        // no marker: retried on the next start; this start the Tinder view opens logged out
        log(
            user_data,
            &format!("Login do Tinder não pôde ser migrado do Electron; entre novamente ({e})"),
        );
        //tell the user on screen, not only in the log
        super::os::toast(
            "Não consegui trazer seu login do Tinder da versão anterior. Entre novamente no Tinder.",
            icon,
        );
    }
}

/// Phase 2: stage a file-level copy (the sources are never opened as databases), then swap it in.
/// An interrupted run leaves no marker and is redone in full next start.
fn copy(src: &Path, udf: &Path, profile: &Path) -> io::Result<()> {
    let staging = udf.join(STAGING);
    remove(&staging)?;
    for rel in STORES {
        copy_tree(&src.join(rel), &staging.join(rel))?;
    }
    for rel in STORES {
        let (from, to) = (staging.join(rel), profile.join(rel));
        remove(&to)?;
        if from.exists() {
            if let Some(dir) = to.parent() {
                fs::create_dir_all(dir)?;
            }
            fs::rename(&from, &to)?;
        }
    }
    remove(&staging)?;
    fs::write(udf.join(MARKER), profile.display().to_string())
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    let meta = match fs::metadata(from) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        m => m?,
    };
    if meta.is_file() {
        if let Some(dir) = to.parent() {
            fs::create_dir_all(dir)?;
        }
        return fs::copy(from, to).map(drop);
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        // LevelDB's lock belongs to whoever opens the copy
        if entry.file_name() != "LOCK" {
            copy_tree(&entry.path(), &to.join(entry.file_name()))?;
        }
    }
    Ok(())
}

fn remove(p: &Path) -> io::Result<()> {
    let r = match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(p),
        Ok(_) => fs::remove_file(p),
        Err(e) => Err(e),
    };
    match r {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        r => r,
    }
}

/// Phase 1: only WebView2 knows where the `tinder` profile lives (the layout is undocumented). Open it once,
/// read `ProfilePath`, then close it and wait until the browser process has exited and released its files.
fn profile_path(udf: &Path, hwnd: HWND) -> io::Result<PathBuf> {
    let options = CoreWebView2EnvironmentOptions::default();
    if cfg!(feature = "test-hooks") {
        // SAFETY: plain setter on our own options object.
        unsafe {
            options.set_additional_browser_arguments("--proxy-server=http://127.0.0.1:9".into())
        };
    }
    let (tx, rx) = mpsc::channel();
    // SAFETY: UI thread (STA); WebView2 owns the handler; the result is awaited below.
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            &HSTRING::from(udf),
            &ICoreWebView2EnvironmentOptions::from(options),
            &CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |hr, env| {
                let _ = tx.send(hr.and_then(|_| env.ok_or_else(|| E_POINTER.into())));
                Ok(())
            })),
        )?;
    }
    let env = pump(&rx)??;
    let (tx, exited) = mpsc::channel();
    let (ctx, crx) = mpsc::channel();
    // SAFETY: as above.
    unsafe {
        let mut token = 0i64;
        env.cast::<ICoreWebView2Environment5>()?
            .add_BrowserProcessExited(
                &BrowserProcessExitedEventHandler::create(Box::new(move |_, _| {
                    let _ = tx.send(());
                    Ok(())
                })),
                &mut token,
            )?;
        let env10 = env.cast::<ICoreWebView2Environment10>()?;
        let opts = env10.CreateCoreWebView2ControllerOptions()?;
        opts.SetProfileName(&HSTRING::from("tinder"))?;
        env10.CreateCoreWebView2ControllerWithOptions(
            hwnd,
            &opts,
            &CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |hr, c| {
                let _ = ctx.send(hr.and_then(|_| c.ok_or_else(|| E_POINTER.into())));
                Ok(())
            })),
        )?;
    }
    let controller = pump(&crx)??;
    // SAFETY: UI thread; the string is taken and freed by take_pwstr.
    let path = unsafe {
        let mut p = PWSTR::null();
        let r = controller
            .CoreWebView2()
            .and_then(|w| w.cast::<ICoreWebView2_13>())
            .and_then(|w| w.Profile())
            .and_then(|pr| pr.ProfilePath(&mut p));
        let _ = controller.Close();
        r.map(|_| PathBuf::from(take_pwstr(p)))
    };
    drop(controller);
    pump(&exited)?;
    let path = path?;
    // never write outside the WebView2 folder, whatever the runtime answers
    if !path.starts_with(udf) {
        return Err(io::Error::other(format!(
            "perfil fora da pasta: {}",
            path.display()
        )));
    }
    Ok(path)
}

/// Waits for a WebView2 callback while dispatching this thread's messages (as wry's own waits do), up to 20 s.
fn pump<T>(rx: &mpsc::Receiver<T>) -> io::Result<T> {
    let end = Instant::now() + Duration::from_secs(20);
    let mut msg = MSG::default();
    loop {
        if let Ok(v) = rx.try_recv() {
            return Ok(v);
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        // SAFETY: standard message loop on the calling thread.
        unsafe {
            MsgWaitForMultipleObjects(None, false, left.as_millis() as u32, QS_ALLINPUT);
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

/// One line in the app's own format (core/app.rs `log`); the core cannot see this failure.
fn log(user_data: &Path, msg: &str) {
    use std::io::Write;
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0);
    let line = format!("{}  {msg}\n", crate::core::clock::log_stamp(ms));
    let _ = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(user_data.join("spark.log"))
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_replaces_each_store_and_never_writes_the_source() {
        let root = std::env::temp_dir().join(format!("spark-migrate-{}", std::process::id()));
        let (src, udf) = (root.join("src"), root.join("udf"));
        let profile = udf.join("EBWebView").join("tinder");
        let put = |p: PathBuf, s: &str| {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, s).unwrap();
        };
        put(src.join("Network\\Cookies"), "db");
        put(src.join("Local Storage\\leveldb\\CURRENT"), "cur");
        put(src.join("Local Storage\\leveldb\\LOCK"), "");
        put(profile.join("Network\\Cookies"), "fresh");
        put(profile.join("Network\\Cookies-journal"), "stale");
        put(profile.join("Preferences"), "kept");
        copy(&src, &udf, &profile).unwrap();
        let read = |p: PathBuf| fs::read_to_string(p).ok();
        assert_eq!(
            read(profile.join("Network\\Cookies")).as_deref(),
            Some("db")
        );
        assert_eq!(read(profile.join("Network\\Cookies-journal")), None);
        assert_eq!(
            read(profile.join("Local Storage\\leveldb\\CURRENT")).as_deref(),
            Some("cur")
        );
        assert!(!profile.join("Local Storage\\leveldb\\LOCK").exists());
        assert_eq!(read(profile.join("Preferences")).as_deref(), Some("kept"));
        assert!(udf.join(MARKER).exists() && !udf.join(STAGING).exists());
        assert_eq!(read(src.join("Network\\Cookies")).as_deref(), Some("db"));
        assert!(src.join("Local Storage\\leveldb\\LOCK").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
