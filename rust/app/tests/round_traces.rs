//!the Rust round() reproduces, call for call, the traces the ORIGINAL engine.js round() recorded
//! against the fake driver (tests/fixtures/round-traces.json, recorded from the original engine), same seeded RNG.
mod common;

use std::{cell::RefCell, rc::Rc};

use common::{Calls, FakeOpts, FakeTinder, Recorder, TestEnv};
use serde_json::{Value, json};
use spark::core::{
    engine::{Mulberry32, Reason, RoundOpts, round},
    js::number,
};
use spark_store::port::stringify;

const FIXTURE: &str = include_str!("fixtures/round-traces.json");

async fn replay(sc: &Value) -> Vec<Value> {
    let fake = FakeTinder::new(FakeOpts::from_json(&sc["fake"]));
    if let Some(screen) = sc["screen"].as_str() {
        fake.t.borrow_mut().screen = if screen == "popup" { "popup" } else { "match" };
    }
    fake.stubborn.set(sc["stubborn"] == Value::Bool(true));
    if sc["gone"] == Value::Bool(true) {
        fake.t.borrow_mut().gone = true;
    }
    let calls: Calls = Rc::new(RefCell::new(Vec::new()));
    let rec = Recorder {
        d: &fake,
        calls: calls.clone(),
    };
    let env = TestEnv {
        rng: Mulberry32::new(number(&sc["seed"]).unwrap_or(0.0) as u32),
        calls: Some(calls.clone()),
    };
    let stop_after = number(&sc["stopAfter"]);
    let n = Rc::new(std::cell::Cell::new(0.0));
    let o = &sc["opts"];
    let opts = {
        let (c1, c2, c3, n) = (calls.clone(), calls.clone(), calls.clone(), n.clone());
        RoundOpts {
            skip_first: o["skipFirst"] == Value::Bool(true),
            per_load: number(&o["perLoad"]).map(|f| f as i64),
            delay: [
                number(&o["delay"][0]).unwrap_or(0.0),
                number(&o["delay"][1]).unwrap_or(0.0),
            ],
            stop: Box::new(move || {
                let s = stop_after.is_some_and(|max| {
                    n.set(n.get() + 1.0);
                    n.get() > max
                });
                c1.borrow_mut().push(json!(["stop", s]));
                s
            }),
            on_like: Box::new(move |k| c2.borrow_mut().push(json!(["onLike", k]))),
            on_match: Box::new(move || c3.borrow_mut().push(json!(["onMatch"]))),
        }
    };
    let multi = number(&sc["multi"]).unwrap_or(0.0) as usize;
    let mut results = 0;
    loop {
        if multi > 0 {
            spark::core::engine::Driver::reload(&rec).await;
        }
        let r = round(&rec, &opts, &env).await;
        let entry = match &r {
            Ok(r) => json!(["result", r.to_json()]),
            Err(e) => json!(["error", e]),
        };
        calls.borrow_mut().push(entry);
        results += 1;
        if !(multi > 0 && matches!(r, Ok(x) if x.reason == Reason::Reload) && results < multi) {
            break;
        }
    }
    calls.take()
}

#[tokio::test]
async fn round_reproduces_every_trace() {
    let traces: Vec<Value> = serde_json::from_str(FIXTURE).unwrap();
    assert!(traces.len() >= 80, "the oracle records 91 scenarios");
    let mut total = 0;
    for sc in &traces {
        let want = sc["calls"].as_array().unwrap();
        let got = replay(sc).await;
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert_eq!(stringify(g), stringify(w), "{}: call #{i}", sc["name"]);
        }
        assert_eq!(got.len(), want.len(), "{}: number of calls", sc["name"]);
        total += got.len();
    }
    assert!(total > 8000);
}
