//! Core: port of main.js / engine.js / driver-web.js on the logic thread (contract: `crate::iface`).
pub mod app;
pub mod clock;
pub mod driver_web;
pub mod engine;
pub mod exec;
pub mod geo;
pub mod js;
pub mod retry;

use std::{fs, io::Write, path::PathBuf, rc::Rc, sync::Once};

pub use app::{Deps, run_app};

use crate::iface::{Host, HostEvent};
use clock::Clock;

/// main.js:9-28: a panic (Node: an uncaught exception) is printed and appended to `<userData>/spark.log`; once per
/// process. A panicking task dies alone, the app keeps running (tokio's default).
fn hook_errors(user_data: &std::path::Path) {
    static HOOKED: Once = Once::new();
    let log = user_data.join(app::LOG_NAME);
    HOOKED.call_once(move || {
        std::panic::set_hook(Box::new(move |info| {
            let msg = format!("[CRITICAL] Erro não tratado: {info}");
            eprintln!("{msg}");
            if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&log) {
                let _ = writeln!(f, "{}  {msg}", clock::crash_stamp(clock::SystemClock.now()));
            }
        }));
    });
}

/// The logic thread: main.js's behavior until `app.quit()` (then returns), on a current-thread runtime.
pub fn run_core(
    argv: Vec<String>,
    user_data: PathBuf,
    host: Rc<dyn Host>,
    events: tokio::sync::mpsc::UnboundedReceiver<HostEvent>,
) {
    hook_errors(&user_data);
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return eprintln!("Falha na inicialização do app: {e}"),
    };
    tokio::task::LocalSet::new().block_on(
        &rt,
        run_app(Deps::production(), argv, user_data, host, events),
    );
}
