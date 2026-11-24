//! The Rust port of `engine.js`'s pure helpers `classify` (engine.js:12-18) and `autostartPlan` (engine.js:113-119).
//! The regex tests are the already-ported `spark-patterns` crate (module `port`), not re-ported here.
//!
//! JS semantics kept: `p.text` is handed to `RegExp.test` as-is (a missing `text` is the string "undefined", `null`
//! is "null"), `!!p.login` and `p.dialog` use JS truthiness, and `snap`/`ready`/`url` are passed through
//! (an absent one comes back `undefined`). `p`/`s` of `null`/`undefined` throw a `TypeError` like the original.

#![forbid(unsafe_code)]

use crate::js::{is_undefined, undefined};
use serde_json::{Map, Value};
use spark_patterns::port as pat;

type Test = fn(Value) -> Result<Value, String>;

/// `p.<key>` / `s.<key>`: absent, or a property of a primitive, is `undefined`; `null`/`undefined` throw.
fn prop(v: &Value, key: &str) -> Result<Value, String> {
    if v.is_null() || is_undefined(v) {
        let what = if v.is_null() { "null" } else { "undefined" };
        return Err(format!(
            "TypeError: Cannot read properties of {what} (reading '{key}')"
        ));
    }
    Ok(v.as_object()
        .and_then(|o| o.get(key))
        .cloned()
        .unwrap_or_else(undefined))
}

/// JS truthiness of a JSON value (`undefined` marker included).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Object(_) if is_undefined(v) => false,
        // Record codec: {"$num": "NaN" | "-0" | "Infinity" | "-Infinity"}.
        Value::Object(o) if o.len() == 1 && o.contains_key("$num") => {
            o["$num"].as_str().is_some_and(|n| n.contains("Inf"))
        }
        Value::Object(_) => true,
        Value::Array(_) => true,
    }
}

/// `RE.test(text)` through the patterns crate.
fn hit(re: Test, text: &Value) -> Result<bool, String> {
    Ok(re(text.clone())? == Value::Bool(true))
}

/// Port of `classify` (`classify.js:5`): `'out' | 'empty' | 'match' | 'popup' | 'ok'` plus the passed-through fields.
pub fn classify(p: Value) -> Result<Value, String> {
    let raw = prop(&p, "text")?;
    let text = match &raw {
        r if is_undefined(r) => Value::String("undefined".into()),
        // Record codec for a string with lone surrogates: patterns never match them, U+FFFD stands in.
        Value::Object(o) if o.len() == 1 && o.contains_key("$utf16") => Value::String(
            char::decode_utf16(
                o["$utf16"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64)
                    .map(|u| u as u16),
            )
            .map(|c| c.unwrap_or('\u{FFFD}'))
            .collect(),
        ),
        _ => raw,
    };
    let out =
        hit(pat::out_re, &text)? || (hit(pat::likes_offer_re, &text)? && hit(pat::plan_re, &text)?);
    let s = if out {
        "out"
    } else if hit(pat::empty_re, &text)? {
        "empty"
    } else if hit(pat::match_re, &text)? {
        "match"
    } else if truthy(&prop(&p, "dialog")?) {
        "popup"
    } else {
        "ok"
    };
    let mut o = Map::new();
    o.insert("s".into(), s.into());
    for k in ["snap", "ready", "url"] {
        o.insert(k.into(), prop(&p, k)?);
    }
    o.insert("login".into(), truthy(&prop(&p, "login")?).into());
    Ok(Value::Object(o))
}

/// Port of `autostartPlan` (`classify.js:6`): Windows' entry wins; with none, the saved option decides.
pub fn autostart_plan(s: Value, wanted: Value) -> Result<Value, String> {
    let mut o = Map::new();
    if truthy(&prop(&s, "openAtLogin")?) {
        o.insert("autostart".into(), prop(&s, "executableWillLaunchAtLogin")?);
        o.insert("register".into(), false.into());
    } else {
        o.insert("autostart".into(), wanted.clone());
        o.insert("register".into(), wanted);
    }
    Ok(Value::Object(o))
}

/// Plain Rust entry points for the app (not bridged: the bridge only exposes the top-level functions above).
pub mod plain {
    /// For the app: `classify` of a probe object (errors only when `p` is `null`/`undefined`).
    pub fn classify_value(p: &serde_json::Value) -> Result<serde_json::Value, String> {
        super::classify(p.clone())
    }

    /// For the app: `autostartPlan(s, wanted)` on JSON values.
    pub fn autostart_plan_value(
        s: &serde_json::Value,
        wanted: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        super::autostart_plan(s.clone(), wanted.clone())
    }

    /// For the app: `autostartPlan` on plain booleans -> `(autostart, register)`.
    pub fn autostart_plan_bool(
        open_at_login: bool,
        will_launch: bool,
        wanted: bool,
    ) -> (bool, bool) {
        if open_at_login {
            (will_launch, false)
        } else {
            (wanted, wanted)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn s(text: &str) -> String {
        classify(json!({"text": text})).unwrap()["s"]
            .as_str()
            .unwrap()
            .into()
    }

    #[test]
    fn screens_and_precedence() {
        assert_eq!(s("You're out of Likes"), "out");
        assert_eq!(s("Curtidas ilimitadas. Escolha um plano"), "out");
        assert_eq!(s("Curtidas ilimitadas"), "ok");
        assert_eq!(s("There's no one new around you"), "empty");
        assert_eq!(s("It's a Match!"), "match");
        assert_eq!(s("no one new. it's a match"), "empty");
        assert_eq!(s("out of likes. no one new"), "out");
        assert_eq!(
            classify(json!({"text": "", "dialog": 1})).unwrap()["s"],
            "popup"
        );
    }

    #[test]
    fn autostart() {
        assert_eq!(
            autostart_plan(
                json!({"openAtLogin": true, "executableWillLaunchAtLogin": false}),
                json!(true)
            )
            .unwrap(),
            json!({"autostart": false, "register": false})
        );
        assert_eq!(
            autostart_plan(json!({"openAtLogin": false}), json!(true)).unwrap(),
            json!({"autostart": true, "register": true})
        );
        assert!(autostart_plan(Value::Null, json!(1)).is_err());
        assert_eq!(plain::autostart_plan_bool(false, false, true), (true, true));
    }
}
