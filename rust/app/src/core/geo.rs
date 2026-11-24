//! main.js:401-418 `windowsGeo()`: the Windows position through PowerShell's GeoCoordinateWatcher, cached 30 min,
//! no new PowerShell for 5 min after an attempt, one request at a time; memory only, never logged (S-07.8).
use std::{cell::RefCell, rc::Rc};

use serde_json::Value;
use tokio::sync::oneshot;

use super::{clock::Clock, driver_web::scripts, exec::Exec, js};
use crate::iface::Fut;

const FRESH_MS: f64 = 30.0 * 60_000.0;
const BACKOFF_MS: f64 = 5.0 * 60_000.0;
const TIMEOUT_MS: u64 = 15_000;

#[derive(Default)]
struct State {
    geo: Option<Value>,
    at: f64,
    tried: f64,
    /// `geoReq`: the waiters of the request in flight.
    req: Option<Vec<oneshot::Sender<Option<Value>>>>,
}

pub struct GeoCache {
    st: Rc<RefCell<State>>,
    clock: Rc<dyn Clock>,
    exec: Rc<dyn Exec>,
}

/// `{lat, lon, acc}` as numbers for the CDP override.
pub fn coords(g: &Value) -> (f64, f64, f64) {
    let n = |k: &str| g.get(k).map_or(f64::NAN, js::to_number);
    (n("lat"), n("lon"), n("acc"))
}

/// `const g = JSON.parse(out); if (isFinite(g.lat) && isFinite(g.lon)) ...` (a throw = no new value).
fn parse(out: &str) -> Option<Value> {
    let g = spark_store::port::parse(out).ok()?;
    let finite = |k: &str| g.get(k).is_some_and(|v| js::to_number(v).is_finite());
    (finite("lat") && finite("lon")).then_some(g)
}

impl GeoCache {
    pub fn new(clock: Rc<dyn Clock>, exec: Rc<dyn Exec>) -> Self {
        GeoCache {
            st: Rc::default(),
            clock,
            exec,
        }
    }

    /// `windowsGeo()`.
    pub fn get(&self) -> Fut<'static, Option<Value>> {
        let now = self.clock.now();
        let mut st = self.st.borrow_mut();
        if st.geo.is_some() && now - st.at < FRESH_MS {
            let g = st.geo.clone();
            return Box::pin(async move { g });
        }
        if st.req.is_none() && now - st.tried < BACKOFF_MS {
            let g = st.geo.clone();
            return Box::pin(async move { g });
        }
        if st.req.is_none() {
            st.tried = now;
            st.req = Some(Vec::new());
            let (state, clock, exec) = (self.st.clone(), self.clock.clone(), self.exec.clone());
            let script = scripts().map(|s| s.geo_ps.clone()).unwrap_or_default();
            tokio::task::spawn_local(async move {
                let args = ["-NoProfile", "-NonInteractive", "-Command", script.as_str()]
                    .map(String::from);
                let out = exec.exec("powershell.exe", &args, TIMEOUT_MS).await;
                let mut st = state.borrow_mut();
                if let Some(g) = parse(&out.stdout) {
                    st.geo = Some(g);
                    st.at = clock.now();
                }
                let geo = st.geo.clone();
                for w in st.req.take().unwrap_or_default() {
                    let _ = w.send(geo.clone());
                }
            });
        }
        let (tx, rx) = oneshot::channel();
        if let Some(req) = st.req.as_mut() {
            req.push(tx);
        }
        Box::pin(async move { rx.await.unwrap_or(None) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{clock::MockClock, exec::ExecOutput};
    use std::{cell::Cell, time::Duration};

    struct FakeExec(Rc<Cell<u32>>, &'static str);
    impl Exec for FakeExec {
        fn exec(&self, file: &str, args: &[String], timeout_ms: u64) -> Fut<'static, ExecOutput> {
            assert_eq!(
                (file, args[2].as_str(), timeout_ms),
                ("powershell.exe", "-Command", 15_000)
            );
            self.0.set(self.0.get() + 1);
            let stdout = self.1.to_string();
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                ExecOutput {
                    stdout,
                    ..Default::default()
                }
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn caches_and_backs_off() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let clock = Rc::new(MockClock::new(1e12));
                let runs = Rc::new(Cell::new(0));
                let g = GeoCache::new(
                    clock.clone(),
                    Rc::new(FakeExec(
                        runs.clone(),
                        "{\"lat\":-23.5,\"lon\":-46.6,\"acc\":30}\r\n",
                    )),
                );
                let (a, b) = (g.get(), g.get());
                assert_eq!(a.await, b.await);
                assert_eq!(runs.get(), 1, "one request at a time");
                assert_eq!(
                    g.get().await.map(|v| coords(&v)),
                    Some((-23.5, -46.6, 30.0))
                );
                clock.set_time(1e12 + 31.0 * 60_000.0);
                g.get().await;
                assert_eq!(runs.get(), 2, "refreshed after 30 min");
                let none = GeoCache::new(clock.clone(), Rc::new(FakeExec(runs.clone(), "")));
                assert_eq!(none.get().await, None);
                assert_eq!(none.get().await, None);
                assert_eq!(runs.get(), 3, "no new PowerShell within 5 min of a failure");
            })
            .await;
    }
}
