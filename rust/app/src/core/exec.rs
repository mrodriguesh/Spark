//! `child_process.execFile(file, args, { windowsHide: true, timeout }, (error, stdout, stderr) => ...)`.
use std::{process::Stdio, time::Duration};

use crate::iface::Fut;

/// What execFile's callback receives: `error` is `error.message` (None = success).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExecOutput {
    pub error: Option<String>,
    pub stdout: String,
    pub stderr: String,
}

pub trait Exec {
    fn exec(&self, file: &str, args: &[String], timeout_ms: u64) -> Fut<'static, ExecOutput>;
}

/// Production: a hidden child process (CREATE_NO_WINDOW), killed after `timeout_ms`.
pub struct ProcessExec;

impl Exec for ProcessExec {
    fn exec(&self, file: &str, args: &[String], timeout_ms: u64) -> Fut<'static, ExecOutput> {
        let (file, args) = (file.to_string(), args.to_vec());
        Box::pin(async move {
            // Node: `Command failed: <file> <args joined by spaces>\n<stderr>`
            let failed = |stderr: &str| {
                format!(
                    "Command failed: {}\n{stderr}",
                    std::iter::once(&file)
                        .chain(&args)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            };
            let mut cmd = tokio::process::Command::new(&file);
            cmd.args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            #[cfg(windows)]
            cmd.creation_flags(0x0800_0000);
            let child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let code = if e.kind() == std::io::ErrorKind::NotFound {
                        "ENOENT".to_string()
                    } else {
                        e.to_string()
                    };
                    return ExecOutput {
                        error: Some(format!("spawn {file} {code}")),
                        ..Default::default()
                    };
                }
            };
            // shortcut: on timeout the partial stdout/stderr is dropped with the killed child (Node keeps it)
            match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output())
                .await
            {
                Ok(Ok(out)) => {
                    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
                    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
                    let error = (!out.status.success()).then(|| failed(&stderr));
                    ExecOutput {
                        error,
                        stdout,
                        stderr,
                    }
                }
                Ok(Err(e)) => ExecOutput {
                    error: Some(e.to_string()),
                    ..Default::default()
                },
                Err(_) => ExecOutput {
                    error: Some(failed("")),
                    ..Default::default()
                },
            }
        })
    }
}
