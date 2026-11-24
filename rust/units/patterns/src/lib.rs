//! JS bindings for `patterns.js`: generated from the signatures in `port.rs` by rust-anything.
//! Regenerate with `rust-anything refresh` after changing them.

use wasm_bindgen::prelude::*;

pub mod js;
pub mod port;
pub mod wire;

#[allow(unused_imports)]
use serde_json::Value;

/// `outRe`.
fn out_re_inner(text: String) -> Result<String, String> {
    let text = crate::js::from_json(&text)?;
    let out = port::out_re(text)?;
    crate::js::to_json(&out)
}

#[wasm_bindgen(js_name = "outRe")]
pub fn out_re(text: String) -> Result<String, JsError> {
    out_re_inner(text).map_err(|e| JsError::new(&e))
}

/// `likesOfferRe`.
fn likes_offer_re_inner(text: String) -> Result<String, String> {
    let text = crate::js::from_json(&text)?;
    let out = port::likes_offer_re(text)?;
    crate::js::to_json(&out)
}

#[wasm_bindgen(js_name = "likesOfferRe")]
pub fn likes_offer_re(text: String) -> Result<String, JsError> {
    likes_offer_re_inner(text).map_err(|e| JsError::new(&e))
}

/// `planRe`.
fn plan_re_inner(text: String) -> Result<String, String> {
    let text = crate::js::from_json(&text)?;
    let out = port::plan_re(text)?;
    crate::js::to_json(&out)
}

#[wasm_bindgen(js_name = "planRe")]
pub fn plan_re(text: String) -> Result<String, JsError> {
    plan_re_inner(text).map_err(|e| JsError::new(&e))
}

/// `emptyRe`.
fn empty_re_inner(text: String) -> Result<String, String> {
    let text = crate::js::from_json(&text)?;
    let out = port::empty_re(text)?;
    crate::js::to_json(&out)
}

#[wasm_bindgen(js_name = "emptyRe")]
pub fn empty_re(text: String) -> Result<String, JsError> {
    empty_re_inner(text).map_err(|e| JsError::new(&e))
}

/// `matchRe`.
fn match_re_inner(text: String) -> Result<String, String> {
    let text = crate::js::from_json(&text)?;
    let out = port::match_re(text)?;
    crate::js::to_json(&out)
}

#[wasm_bindgen(js_name = "matchRe")]
pub fn match_re(text: String) -> Result<String, JsError> {
    match_re_inner(text).map_err(|e| JsError::new(&e))
}

/// `dismissRe`.
fn dismiss_re_inner(text: String) -> Result<String, String> {
    let text = crate::js::from_json(&text)?;
    let out = port::dismiss_re(text)?;
    crate::js::to_json(&out)
}

#[wasm_bindgen(js_name = "dismissRe")]
pub fn dismiss_re(text: String) -> Result<String, JsError> {
    dismiss_re_inner(text).map_err(|e| JsError::new(&e))
}

// ---- the binary wire ------------------------------------------------------------------------

thread_local! {
    static WIRE_IN: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    static WIRE_OUT: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// This module's memory, where `index.cjs` reads and writes the wire buffers.
#[wasm_bindgen(js_name = "rustAnythingWireMemory")]
pub fn rust_anything_wire_memory() -> JsValue {
    wasm_bindgen::memory()
}

/// The address of an argument buffer of `len` bytes.
#[wasm_bindgen(js_name = "rustAnythingWireIn")]
pub fn rust_anything_wire_in(len: u32) -> u32 {
    WIRE_IN.with(|b| {
        let mut b = b.borrow_mut();
        b.resize(len as usize, 0);
        b.as_mut_ptr() as usize as u32
    })
}

/// The address of the last result.
#[wasm_bindgen(js_name = "rustAnythingWireOut")]
pub fn rust_anything_wire_out() -> u32 {
    WIRE_OUT.with(|b| b.borrow().as_ptr() as usize as u32)
}

/// Reads `len` argument bytes, runs `f`, and keeps its result for `rustAnythingWireOut`.
fn wire_io(
    len: u32,
    f: impl FnOnce(&mut wire::Reader, &mut wire::Writer) -> Result<(), String>,
) -> Result<u32, JsError> {
    let input = WIRE_IN.with(|b| std::mem::take(&mut *b.borrow_mut()));
    let mut w = wire::Writer::default();
    let result = f(
        &mut wire::Reader::new(&input[..(len as usize).min(input.len())]),
        &mut w,
    );
    WIRE_IN.with(|b| *b.borrow_mut() = input);
    match result {
        // A result of another shape than the TS type: JS runs the call through JSON.
        Err(e) if e.starts_with(wire::MISMATCH) => return Ok(u32::MAX),
        other => other.map_err(|e| JsError::new(&e))?,
    }
    let n = w.0.len();
    WIRE_OUT.with(|b| *b.borrow_mut() = w.0);
    Ok(u32::try_from(n).unwrap_or(u32::MAX))
}

static S_OUT_RE_0: wire::Schema = wire::Schema::Dyn;
static S_OUT_RE_OUT: wire::Schema = wire::Schema::Dyn;

/// `outRe` over the binary wire.
#[wasm_bindgen(js_name = "outReWire")]
pub fn out_re_wire(len: u32) -> Result<u32, JsError> {
    wire_io(len, |r, w| {
        let text: Value = wire::read_value(r, &S_OUT_RE_0)?;
        let out = &port::out_re(text)?;
        wire::write_value(w, out, &S_OUT_RE_OUT)?;
        Ok(())
    })
}

static S_LIKES_OFFER_RE_0: wire::Schema = wire::Schema::Dyn;
static S_LIKES_OFFER_RE_OUT: wire::Schema = wire::Schema::Dyn;

/// `likesOfferRe` over the binary wire.
#[wasm_bindgen(js_name = "likesOfferReWire")]
pub fn likes_offer_re_wire(len: u32) -> Result<u32, JsError> {
    wire_io(len, |r, w| {
        let text: Value = wire::read_value(r, &S_LIKES_OFFER_RE_0)?;
        let out = &port::likes_offer_re(text)?;
        wire::write_value(w, out, &S_LIKES_OFFER_RE_OUT)?;
        Ok(())
    })
}

static S_PLAN_RE_0: wire::Schema = wire::Schema::Dyn;
static S_PLAN_RE_OUT: wire::Schema = wire::Schema::Dyn;

/// `planRe` over the binary wire.
#[wasm_bindgen(js_name = "planReWire")]
pub fn plan_re_wire(len: u32) -> Result<u32, JsError> {
    wire_io(len, |r, w| {
        let text: Value = wire::read_value(r, &S_PLAN_RE_0)?;
        let out = &port::plan_re(text)?;
        wire::write_value(w, out, &S_PLAN_RE_OUT)?;
        Ok(())
    })
}

static S_EMPTY_RE_0: wire::Schema = wire::Schema::Dyn;
static S_EMPTY_RE_OUT: wire::Schema = wire::Schema::Dyn;

/// `emptyRe` over the binary wire.
#[wasm_bindgen(js_name = "emptyReWire")]
pub fn empty_re_wire(len: u32) -> Result<u32, JsError> {
    wire_io(len, |r, w| {
        let text: Value = wire::read_value(r, &S_EMPTY_RE_0)?;
        let out = &port::empty_re(text)?;
        wire::write_value(w, out, &S_EMPTY_RE_OUT)?;
        Ok(())
    })
}

static S_MATCH_RE_0: wire::Schema = wire::Schema::Dyn;
static S_MATCH_RE_OUT: wire::Schema = wire::Schema::Dyn;

/// `matchRe` over the binary wire.
#[wasm_bindgen(js_name = "matchReWire")]
pub fn match_re_wire(len: u32) -> Result<u32, JsError> {
    wire_io(len, |r, w| {
        let text: Value = wire::read_value(r, &S_MATCH_RE_0)?;
        let out = &port::match_re(text)?;
        wire::write_value(w, out, &S_MATCH_RE_OUT)?;
        Ok(())
    })
}

static S_DISMISS_RE_0: wire::Schema = wire::Schema::Dyn;
static S_DISMISS_RE_OUT: wire::Schema = wire::Schema::Dyn;

/// `dismissRe` over the binary wire.
#[wasm_bindgen(js_name = "dismissReWire")]
pub fn dismiss_re_wire(len: u32) -> Result<u32, JsError> {
    wire_io(len, |r, w| {
        let text: Value = wire::read_value(r, &S_DISMISS_RE_0)?;
        let out = &port::dismiss_re(text)?;
        wire::write_value(w, out, &S_DISMISS_RE_OUT)?;
        Ok(())
    })
}
