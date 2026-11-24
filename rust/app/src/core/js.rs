//! The JS value semantics main.js / engine.js rely on, over `serde_json::Value` (which, with the store unit's
//! `arbitrary_precision`, keeps numbers as text: read them through `number`, write them through `num`).
use serde_json::{Number, Value};

/// A JSON number as the JS double it denotes.
pub fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_str().parse::<f64>().ok(),
        _ => None,
    }
}

/// A JS number as a JSON value; NaN and the infinities have no JSON form (JSON.stringify writes `null`).
pub fn num(f: f64) -> Value {
    if f.is_finite() && f == f.trunc() && f.abs() <= 9_007_199_254_740_991.0 {
        Value::from(f as i64)
    } else {
        Number::from_f64(f).map_or(Value::Null, Value::Number)
    }
}

/// JS truthiness (`!!v`).
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(_) => number(v).is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `a === b` for values that came from JSON: objects and arrays are never the same reference.
pub fn strict_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Number(_), Value::Number(_)) => number(a) == number(b),
        (Value::String(x), Value::String(y)) => x == y,
        _ => false,
    }
}

/// `===` where `None` is `undefined`.
pub fn strict_eq_opt(a: Option<&Value>, b: Option<&Value>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => strict_eq(a, b),
        _ => false,
    }
}

/// `typeof v` (`None` = `undefined`).
pub fn type_of(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Null | Value::Array(_) | Value::Object(_)) => "object",
    }
}

/// `Number.isSafeInteger(f)`.
pub fn is_safe_integer(f: f64) -> bool {
    f.is_finite() && f == f.trunc() && f.abs() <= 9_007_199_254_740_991.0
}

/// ECMAScript WhiteSpace + LineTerminator (what `trim` and `Number(string)` strip).
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// `String.prototype.trim`.
pub fn trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// `Number(string)` (StringToNumber).
pub fn string_to_number(s: &str) -> f64 {
    let s = trim(s);
    if s.is_empty() {
        return 0.0;
    }
    let radix = match s.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => 10,
    };
    if radix != 10 {
        let digits = &s[2..];
        if digits.is_empty() {
            return f64::NAN;
        }
        return digits
            .chars()
            .try_fold(0.0, |n, c| {
                c.to_digit(radix)
                    .map(|d| n * f64::from(radix) + f64::from(d))
            })
            .unwrap_or(f64::NAN);
    }
    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
    if unsigned == "Infinity" {
        return if s.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    // StrDecimalLiteral: digits, one optional '.', optional exponent; Rust's parser also takes "inf"/"nan", JS not
    let mut parts = unsigned.splitn(2, ['e', 'E']);
    let mantissa = parts.next().unwrap_or("");
    let exponent_ok = parts.next().is_none_or(|e| {
        let d = e.strip_prefix(['+', '-']).unwrap_or(e);
        !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
    });
    let dots = mantissa.bytes().filter(|b| *b == b'.').count();
    let mantissa_ok = dots <= 1
        && mantissa.bytes().any(|b| b.is_ascii_digit())
        && mantissa.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    if !(exponent_ok && mantissa_ok) {
        return f64::NAN;
    }
    let text = if mantissa.ends_with('.') {
        s.replacen('.', "", 1)
    } else {
        s.to_string()
    };
    text.parse::<f64>().unwrap_or(f64::NAN)
}

/// `Number#toString()`.
pub fn number_to_string(f: f64) -> String {
    spark_store::js::number_to_string(f)
}

/// `String(v)` for JSON values (arrays join with `,`, `null`/`undefined` elements as empty).
pub fn to_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(_) => number(v).map_or_else(|| "NaN".into(), number_to_string),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|e| {
                if e.is_null() {
                    String::new()
                } else {
                    to_string(e)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// `Number(v)` (ToNumber) for JSON values.
pub fn to_number(v: &Value) -> f64 {
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(_) => number(v).unwrap_or(f64::NAN),
        Value::String(s) => string_to_number(s),
        Value::Array(_) => string_to_number(&to_string(v)),
        Value::Object(_) => f64::NAN,
    }
}

/// `v + d` with `d` a number: a string (or an array/object, through its string) concatenates.
pub fn plus(v: &Value, d: f64) -> Value {
    match v {
        Value::String(_) | Value::Array(_) | Value::Object(_) => {
            Value::String(to_string(v) + &number_to_string(d))
        }
        _ => num(to_number(v) + d),
    }
}

/// `Math.max(0, x)` of a JS value (ToNumber, NaN wins).
pub fn max0(v: &Value) -> f64 {
    let f = to_number(v);
    if f.is_nan() { f } else { f.max(0.0) }
}

/// An error's `message` from a unit's `Name: message` text (units report thrown errors that way).
pub fn message(e: &str) -> String {
    for name in ["TypeError: ", "RangeError: ", "SyntaxError: ", "Error: "] {
        if let Some(m) = e.strip_prefix(name) {
            return m.to_string();
        }
    }
    e.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn js_semantics() {
        for (s, n) in [
            ("", 0.0),
            (" 12 ", 12.0),
            ("1e3", 1000.0),
            ("0x10", 16.0),
            ("-1.5", -1.5),
            (".5", 0.5),
            ("5.", 5.0),
            ("1795363520260", 1_795_363_520_260.0),
        ] {
            assert_eq!(string_to_number(s), n, "{s:?}");
        }
        for s in ["abc", "1e", "inf", "NaN", "1.2.3", "0x", "--1", "1_0"] {
            assert!(string_to_number(s).is_nan(), "{s:?}");
        }
        assert_eq!(string_to_number("-Infinity"), f64::NEG_INFINITY);
        assert!(
            truthy(&json!(1))
                && !truthy(&json!(0))
                && !truthy(&json!(""))
                && truthy(&json!([]))
                && !truthy(&Value::Null)
        );
        assert!(
            strict_eq(&json!(15), &num(15.0))
                && !strict_eq(&json!({}), &json!({}))
                && !strict_eq(&json!("1"), &json!(1))
        );
        assert_eq!(type_of(Some(&Value::Null)), "object");
        assert_eq!(plus(&json!("5"), 1.0), json!("51"));
        assert_eq!(max0(&plus(&json!("5"), 1.0)), 51.0);
        assert_eq!(trim("\u{feff} a\u{85}"), "a\u{85}");
        assert!(
            is_safe_integer(9_007_199_254_740_991.0)
                && !is_safe_integer(0.5)
                && !is_safe_integer(f64::NAN)
        );
        assert_eq!(message("TypeError: x"), "x");
    }
}
