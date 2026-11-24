//! JS bindings for `scheduler.js`: generated from the signatures in `port.rs` by rust-anything.
//! Regenerate with `rust-anything refresh` after changing them.

use napi_derive::napi;

pub mod js;
pub mod port;
pub mod wire;

#[allow(unused_imports)]
use serde_json::Value;

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into())
}

#[allow(dead_code)]
fn run<T>(f: impl FnOnce() -> Result<T, String>) -> napi::Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(result) => result.map_err(napi::Error::from_reason),
        Err(payload) => Err(napi::Error::from_reason(format!("Rust panic: {}", panic_message(&*payload)))),
    }
}

/// `ps`.
#[napi(js_name = "ps")]
pub fn ps(value: String) -> napi::Result<String> {
    run(move || {
        let value = crate::js::from_json(&value)?;
        let out = port::ps(value)?;
        crate::js::to_json(&out)
    })
}

/// `quote`.
#[napi(js_name = "quote")]
pub fn quote(value: String) -> napi::Result<String> {
    run(move || {
        let value = crate::js::from_json(&value)?;
        let out = port::quote(value)?;
        crate::js::to_json(&out)
    })
}

/// `taskName`.
#[napi(js_name = "taskName")]
pub fn task_name(data: String) -> napi::Result<String> {
    run(move || {
        let data = crate::js::from_json(&data)?;
        let out = port::task_name(data)?;
        crate::js::to_json(&out)
    })
}

/// `scheduleCall`.
#[napi(js_name = "scheduleCall")]
pub fn schedule_call(exe: String, data: String, launch_args: String, at: String, answer: String) -> napi::Result<String> {
    run(move || {
        let exe = crate::js::from_json(&exe)?;
        let data = crate::js::from_json(&data)?;
        let launch_args = crate::js::from_json(&launch_args)?;
        let at = crate::js::from_json(&at)?;
        let answer = crate::js::from_json(&answer)?;
        let out = port::schedule_call(exe, data, launch_args, at, answer)?;
        crate::js::to_json(&out)
    })
}

/// `cancelCall`.
#[napi(js_name = "cancelCall")]
pub fn cancel_call(data: String, answer: String) -> napi::Result<String> {
    run(move || {
        let data = crate::js::from_json(&data)?;
        let answer = crate::js::from_json(&answer)?;
        let out = port::cancel_call(data, answer)?;
        crate::js::to_json(&out)
    })
}

// ---- the binary wire ------------------------------------------------------------------------

type WireBuffers = (napi::bindgen_prelude::Uint8Array, napi::bindgen_prelude::Uint8Array, napi::bindgen_prelude::Uint32Array);

thread_local! {
    static WIRE: std::cell::RefCell<Option<WireBuffers>> = const { std::cell::RefCell::new(None) };
    static PENDING: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    static SCRATCH: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Registers the argument, result and argument-length buffers `index.cjs` uses.
#[napi(js_name = "rustAnythingWireInit")]
pub fn rust_anything_wire_init(
    input: napi::bindgen_prelude::Uint8Array,
    output: napi::bindgen_prelude::Uint8Array,
    len: napi::bindgen_prelude::Uint32Array,
) {
    WIRE.with(|w| *w.borrow_mut() = Some((input, output, len)));
}

/// Copies a result that did not fit into the (now larger) result buffer.
#[napi(js_name = "rustAnythingWireFlush")]
pub fn rust_anything_wire_flush() -> f64 {
    let bytes = PENDING.with(|p| std::mem::take(&mut *p.borrow_mut()));
    WIRE.with(|w| {
        let mut w = w.borrow_mut();
        let Some((_, output, _)) = w.as_mut() else {
            return -1.0;
        };
        // SAFETY: the call is synchronous; JS does not touch `output` until it returns.
        let out = unsafe { output.as_mut() };
        let n = bytes.len().min(out.len());
        out[..n].copy_from_slice(&bytes[..n]);
        n as f64
    })
}

/// Reads the argument bytes (`wLen[0]` of them), runs `f`, and writes its result; a negative
/// return is the size of a result that did not fit, kept for `rustAnythingWireFlush`.
fn wire_io(f: impl FnOnce(&mut wire::Reader, &mut wire::Writer) -> Result<(), String>) -> napi::Result<f64> {
    WIRE.with(|cell| {
        let mut guard = cell.borrow_mut();
        let Some((input, output, len)) = guard.as_mut() else {
            return Err(napi::Error::from_reason("rust-anything: the wire buffers are not registered"));
        };
        let len = (len.first().copied().unwrap_or(0) as usize).min(input.len());
        let mut w = wire::Writer(SCRATCH.with(|s| std::mem::take(&mut *s.borrow_mut())));
        w.0.clear();
        let result = run(|| f(&mut wire::Reader::new(&input[..len]), &mut w));
        let keep = |w: wire::Writer| SCRATCH.with(|s| *s.borrow_mut() = w.0);
        match result {
            // A result of another shape than the TS type: JS runs the call through JSON.
            Err(e) if e.reason.starts_with(wire::MISMATCH) => {
                keep(w);
                return Ok(f64::NAN);
            }
            Err(e) => {
                keep(w);
                let mut m = wire::Writer::default();
                m.string16(&e.reason);
                // SAFETY: the call is synchronous; JS does not touch `output` until it returns.
                let out = unsafe { output.as_mut() };
                if m.0.len() > out.len() {
                    return Err(e);
                }
                out[..m.0.len()].copy_from_slice(&m.0);
                return Ok(f64::INFINITY);
            }
            Ok(()) => {}
        }
        // SAFETY: the call is synchronous; JS does not touch `output` until it returns.
        let out = unsafe { output.as_mut() };
        let n = w.0.len();
        if n > out.len() {
            PENDING.with(|p| *p.borrow_mut() = w.0);
            return Ok(-(n as f64));
        }
        out[..n].copy_from_slice(&w.0);
        keep(w);
        Ok(n as f64)
    })
}

static S_PS_0: wire::Schema = wire::Schema::Dyn;
static S_PS_OUT: wire::Schema = wire::Schema::Dyn;

/// `ps` over the binary wire.
#[napi(js_name = "psWire")]
pub fn ps_wire() -> napi::Result<f64> {
    wire_io(|r, w| {
        let value: Value = wire::read_value(r, &S_PS_0)?;
        let out = &port::ps(value)?;
        wire::write_value(w, out, &S_PS_OUT)?;
        Ok(())
    })
}

static S_QUOTE_0: wire::Schema = wire::Schema::Dyn;
static S_QUOTE_OUT: wire::Schema = wire::Schema::Dyn;

/// `quote` over the binary wire.
#[napi(js_name = "quoteWire")]
pub fn quote_wire() -> napi::Result<f64> {
    wire_io(|r, w| {
        let value: Value = wire::read_value(r, &S_QUOTE_0)?;
        let out = &port::quote(value)?;
        wire::write_value(w, out, &S_QUOTE_OUT)?;
        Ok(())
    })
}

static S_TASK_NAME_0: wire::Schema = wire::Schema::Dyn;
static S_TASK_NAME_OUT: wire::Schema = wire::Schema::Dyn;

/// `taskName` over the binary wire.
#[napi(js_name = "taskNameWire")]
pub fn task_name_wire() -> napi::Result<f64> {
    wire_io(|r, w| {
        let data: Value = wire::read_value(r, &S_TASK_NAME_0)?;
        let out = &port::task_name(data)?;
        wire::write_value(w, out, &S_TASK_NAME_OUT)?;
        Ok(())
    })
}

static S_SCHEDULE_CALL_0: wire::Schema = wire::Schema::Dyn;
static S_SCHEDULE_CALL_1: wire::Schema = wire::Schema::Dyn;
static S_SCHEDULE_CALL_2: wire::Schema = wire::Schema::Dyn;
static S_SCHEDULE_CALL_3: wire::Schema = wire::Schema::Dyn;
static S_SCHEDULE_CALL_4: wire::Schema = wire::Schema::Dyn;
static S_SCHEDULE_CALL_OUT: wire::Schema = wire::Schema::Dyn;

/// `scheduleCall` over the binary wire.
#[napi(js_name = "scheduleCallWire")]
pub fn schedule_call_wire() -> napi::Result<f64> {
    wire_io(|r, w| {
        let exe: Value = wire::read_value(r, &S_SCHEDULE_CALL_0)?;
        let data: Value = wire::read_value(r, &S_SCHEDULE_CALL_1)?;
        let launch_args: Value = wire::read_value(r, &S_SCHEDULE_CALL_2)?;
        let at: Value = wire::read_value(r, &S_SCHEDULE_CALL_3)?;
        let answer: Value = wire::read_value(r, &S_SCHEDULE_CALL_4)?;
        let out = &port::schedule_call(exe, data, launch_args, at, answer)?;
        wire::write_value(w, out, &S_SCHEDULE_CALL_OUT)?;
        Ok(())
    })
}

static S_CANCEL_CALL_0: wire::Schema = wire::Schema::Dyn;
static S_CANCEL_CALL_1: wire::Schema = wire::Schema::Dyn;
static S_CANCEL_CALL_OUT: wire::Schema = wire::Schema::Dyn;

/// `cancelCall` over the binary wire.
#[napi(js_name = "cancelCallWire")]
pub fn cancel_call_wire() -> napi::Result<f64> {
    wire_io(|r, w| {
        let data: Value = wire::read_value(r, &S_CANCEL_CALL_0)?;
        let answer: Value = wire::read_value(r, &S_CANCEL_CALL_1)?;
        let out = &port::cancel_call(data, answer)?;
        wire::write_value(w, out, &S_CANCEL_CALL_OUT)?;
        Ok(())
    })
}
