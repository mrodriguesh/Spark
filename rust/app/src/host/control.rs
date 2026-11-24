//! test-only control port (feature `test-hooks`): what the JS tests did through Electron's main-process
//! `--inspect` (`setSize`, `isMinimized`, `getAppMetrics`, `emit('second-instance')`, `capturePage`).
//! One JSON request per line on 127.0.0.1:$SPARK_TEST_CONTROL_PORT, one JSON reply per line.
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpListener,
    sync::mpsc,
    time::Duration,
};

use serde_json::{Value, json};
use tao::event_loop::EventLoopProxy;

use super::Cmd;

pub fn start(proxy: EventLoopProxy<Cmd>) {
    let Some(port) = super::test_setting("SPARK_TEST_CONTROL_PORT") else {
        return;
    };
    let Ok(listener) = TcpListener::bind(format!("127.0.0.1:{port}")) else {
        return;
    };
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let proxy = proxy.clone();
            std::thread::spawn(move || {
                let Ok(clone) = stream.try_clone() else {
                    return;
                };
                let mut out = stream;
                for line in BufReader::new(clone).lines().map_while(Result::ok) {
                    let req: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
                    let (tx, rx) = mpsc::channel();
                    let reply = if proxy.send_event(Cmd::Control(req, tx)).is_ok() {
                        rx.recv_timeout(Duration::from_secs(30))
                            .unwrap_or_else(|_| json!({ "error": "timeout" }))
                    } else {
                        json!({ "error": "app gone" })
                    };
                    if writeln!(out, "{reply}").is_err() {
                        break;
                    }
                }
            });
        }
    });
}
