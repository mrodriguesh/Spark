//! scheduler.js's `retryTask(exe, data)`: the Windows task that reopens Spark after the likes limit (S-08.3..5).
//! Script text, argv and read-back check come from the scheduler unit (`spark_scheduler::port`); the promise
//! queue that serializes the PowerShell calls and `idle()` are here.
use std::rc::Rc;

use serde_json::{Value, json};
use spark_scheduler::port as sched;
use tokio::sync::{Mutex, oneshot};

use super::{
    exec::{Exec, ExecOutput},
    js,
};
use crate::iface::Fut;

/// What main.js uses of the retry task. Each call starts at once (as the JS call does); the future settles like
/// the returned promise.
pub trait RetryTask {
    /// `schedule(at)`: register the task at `at` (ms epoch) and verify it; Err = the error's message.
    fn schedule(&self, at: f64) -> Fut<'static, Result<Value, String>>;
    /// `cancel()`: delete the task if present.
    fn cancel(&self) -> Fut<'static, Result<Value, String>>;
    /// `idle()`: settles when every call made so far has settled.
    fn idle(&self) -> Fut<'static, ()>;
}

const POWERSHELL: &str = "powershell.exe";
const TIMEOUT_MS: u64 = 20_000;

/// Production: PowerShell + `Schedule.Service`, one call at a time.
pub struct PsRetryTask {
    exe: String,
    data: String,
    exec: Rc<dyn Exec>,
    /// scheduler.js's `queue`: a FIFO lock, taken first thing by every call in call order.
    queue: Rc<Mutex<()>>,
}

fn answer(o: ExecOutput) -> Value {
    json!({ "error": o.error.map(|m| json!({ "message": m })), "stdout": o.stdout, "stderr": o.stderr })
}

/// The unit's report of one call -> the promise outcome.
fn outcome(report: Result<Value, String>) -> Result<Value, String> {
    let r = report?;
    if r["ok"] == Value::Bool(true) {
        Ok(r["result"].clone())
    } else {
        Err(r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }
}

fn args_of(report: &Value) -> Option<Vec<String>> {
    report["args"]
        .as_array()?
        .iter()
        .map(|a| a.as_str().map(String::from))
        .collect()
}

impl PsRetryTask {
    pub fn new(exe: String, data: String, exec: Rc<dyn Exec>) -> Self {
        PsRetryTask {
            exe,
            data,
            exec,
            queue: Rc::new(Mutex::new(())),
        }
    }

    /// The unit's pure call with a placeholder answer gives the argv (or the validation error, nothing spawned).
    fn probe(report: Result<Value, String>) -> Result<Vec<String>, String> {
        let r = report?;
        match args_of(&r) {
            Some(args) if r["called"] == Value::Bool(true) => Ok(args),
            _ => Err(outcome(Ok(r)).err().unwrap_or_default()),
        }
    }

    /// scheduler.js `run`: queue = queue.catch(() => {}).then(() => execFile(...)).
    fn run(
        &self,
        args: Vec<String>,
        finish: impl FnOnce(Value) -> Result<Value, String> + 'static,
    ) -> Fut<'static, Result<Value, String>> {
        let (tx, rx) = oneshot::channel();
        let (queue, exec) = (self.queue.clone(), self.exec.clone());
        tokio::task::spawn_local(async move {
            let _turn = queue.lock().await;
            let out = exec.exec(POWERSHELL, &args, TIMEOUT_MS).await;
            let _ = tx.send(finish(answer(out)));
        });
        Box::pin(async move {
            rx.await
                .unwrap_or_else(|_| Err("O Windows não confirmou o agendamento".into()))
        })
    }
}

fn placeholder() -> Value {
    json!({ "error": null, "stdout": "", "stderr": "" })
}

impl RetryTask for PsRetryTask {
    fn schedule(&self, at: f64) -> Fut<'static, Result<Value, String>> {
        let (exe, data, at) = (
            Value::from(self.exe.as_str()),
            Value::from(self.data.as_str()),
            js::num(at),
        );
        let call = move |answer| {
            sched::schedule_call(exe.clone(), data.clone(), json!([]), at.clone(), answer)
        };
        match Self::probe(call(placeholder())) {
            Ok(args) => self.run(args, move |a| outcome(call(a))),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn cancel(&self) -> Fut<'static, Result<Value, String>> {
        let data = Value::from(self.data.as_str());
        let call = move |answer| sched::cancel_call(data.clone(), answer);
        match Self::probe(call(placeholder())) {
            Ok(args) => self.run(args, move |a| outcome(call(a))),
            Err(e) => Box::pin(async move { Err(e) }),
        }
    }

    fn idle(&self) -> Fut<'static, ()> {
        let (tx, rx) = oneshot::channel();
        let queue = self.queue.clone();
        tokio::task::spawn_local(async move {
            drop(queue.lock().await);
            let _ = tx.send(());
        });
        Box::pin(async move {
            let _ = rx.await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Answers each spawn after `delay` ms with the next canned output; records argv and start/end order.
    struct FakeExec {
        log: Rc<RefCell<Vec<String>>>,
        answers: RefCell<Vec<ExecOutput>>,
    }

    impl Exec for FakeExec {
        fn exec(&self, file: &str, args: &[String], timeout_ms: u64) -> Fut<'static, ExecOutput> {
            let n = self
                .log
                .borrow()
                .iter()
                .filter(|l| l.starts_with("start"))
                .count();
            self.log.borrow_mut().push(format!(
                "start {n} {file} {timeout_ms} {}",
                args[..3].join(" ")
            ));
            let out = self.answers.borrow_mut().remove(0);
            let log = self.log.clone();
            Box::pin(async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                log.borrow_mut().push(format!("end {n}"));
                out
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn queue_serializes_and_verifies() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (exe, data) = (r"C:\Spark\Spark.exe", r"C:\Users\x\AppData\Roaming\Spark");
                let at = 1_795_363_520_260.0;
                // the unit's own report for the call it expects to see
                let probe = sched::schedule_call(exe.into(), data.into(), json!([]), js::num(at), placeholder()).unwrap();
                let args = args_of(&probe).unwrap();
                let quoted = format!("\"--user-data-dir={data}\" \"--spark-retry-at={at}\"");
                let good = ExecOutput {
                    stdout: json!({"enabled": true, "exe": exe, "args": quoted, "missed": true, "end": "9999-12-31T00:00:00Z", "logon": 9, "next": 1_795_363_521_000_i64}).to_string(),
                    ..Default::default()
                };
                let log = Rc::new(RefCell::new(Vec::new()));
                let answers = vec![
                    good,
                    ExecOutput { error: Some("Command failed".into()), stderr: "Acesso negado.\r\nlinha 2".into(), ..Default::default() },
                    ExecOutput { stdout: "null\r\n".into(), ..Default::default() },
                ];
                let task = PsRetryTask::new(exe.into(), data.into(), Rc::new(FakeExec { log: log.clone(), answers: RefCell::new(answers) }));
                let (a, b, c) = (task.schedule(at), task.cancel(), task.cancel());
                let idle = task.idle();
                assert_eq!(task.schedule(0.5).await, Err("Horário inválido para a próxima tentativa".into()));
                idle.await;
                assert_eq!(*log.borrow(), ["start 0 powershell.exe 20000 -NoProfile -NonInteractive -EncodedCommand", "end 0", "start 1 powershell.exe 20000 -NoProfile -NonInteractive -EncodedCommand", "end 1", "start 2 powershell.exe 20000 -NoProfile -NonInteractive -EncodedCommand", "end 2"]);
                assert_eq!(a.await.map(|v| v["logon"].clone()), Ok(json!(9)));
                assert_eq!(b.await, Err("Acesso negado.".into()));
                assert_eq!(c.await, Ok(Value::Null));
                assert_eq!(args.len(), 4);
            })
            .await;
    }
}
