use super::*;

/// CommandLineToArgvW for a line of always-quoted arguments (the inverse of `quote_text`).
fn argv(line: &str) -> Vec<String> {
    let (mut out, mut cur, mut quoted, mut slashes) = (vec![], String::new(), false, 0);
    for c in line.chars() {
        match c {
            '\\' => slashes += 1,
            ' ' if !quoted => out.push(std::mem::take(&mut cur)),
            '"' => {
                cur += &"\\".repeat(slashes / 2);
                if slashes % 2 == 1 {
                    cur.push('"');
                } else {
                    quoted = !quoted;
                }
                slashes = 0;
            }
            _ => {
                cur += &"\\".repeat(slashes);
                cur.push(c);
                slashes = 0;
            }
        }
    }
    out.push(cur);
    out
}

const EXE: &str = r"C:\a.exe";
const AT: u64 = 1_795_363_520_260;
const ARGS: &str = r#""--user-data-dir=C:\d" "--spark-retry-at=1795363520260""#;

fn saved() -> Value {
    json!({"enabled": true, "exe": EXE, "args": ARGS, "missed": true, "end": "x", "logon": 9, "next": 1_795_363_521_000_u64})
}

fn answer(stdout: &Value) -> Value {
    json!({"error": null, "stdout": stdout.to_string(), "stderr": ""})
}

fn run(at: Value, a: Value) -> Result<Value, JsErr> {
    schedule(EXE, r"C:\d", &[], &at, |_| a)
}

fn message(r: Result<Value, JsErr>) -> String {
    r.unwrap_err().1
}

#[test]
fn quoting_vectors_from_the_original() {
    assert_eq!(quote_text(r#"--user-data-dir=C:\x\perfil d' teste $"#), r#""--user-data-dir=C:\x\perfil d' teste $""#);
    assert_eq!(quote_text(r#"a\\"b"#), r#""a\\\\\"b""#);
    assert_eq!(quote_text(r"C:\dir\"), r#""C:\dir\\""#);
    assert_eq!(quote_text(""), r#""""#);
    assert_eq!(ps_text("it's $x"), "'it''s $x'");
}

#[test]
fn quoting_round_trips_through_argv_rules() {
    let items = ["--hidden", "", r"C:\a b\", r#"x\\"y"#, "\"", r"\\", "it's $", "João"].map(String::from);
    let line = items.iter().map(|s| quote_text(s)).collect::<Vec<_>>().join(" ");
    assert_eq!(argv(&line), items);
}

#[test]
fn name_hash_and_lowercase() {
    assert_eq!(task_name_of(r"C:\Projects\Spark\test\perfil d' teste $"), "Spark - Intervalo - 9bb462849436f089");
    assert_eq!(task_name_of(r"C:\X\Spark"), task_name_of(r"c:\x\spark"));
}

#[test]
fn utf16_base64_and_iso() {
    let c = call("d", "x");
    assert!(c.args[3].starts_with("dAByAHkAIAB7ACAA"), "base64 of utf16le for try");
    assert_eq!(&c.args[..3], ["-NoProfile", "-NonInteractive", "-EncodedCommand"]);
    assert_eq!(iso_string(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(iso_string(1_795_363_521_000), "2026-11-22T16:05:21.000Z");
    assert_eq!(iso_string(8_640_000_000_000_000), "+275760-09-13T00:00:00.000Z");
    assert_eq!(iso_string(253_402_300_800_000), "+010000-01-01T00:00:00.000Z");
    assert_eq!(iso_string(951_782_400_123), "2000-02-29T00:00:00.123Z");
}

#[test]
fn schedule_validation_never_calls_exec() {
    for bad in [json!(0), json!(-1), json!(1.5), Value::Null, json!(9_007_199_254_740_992_u64), json!("1")] {
        assert_eq!(message(schedule(EXE, "d", &[], &bad, |_| unreachable!())), BAD_TIME);
    }
    let range = schedule(EXE, "d", &[], &json!(8_640_000_000_000_001_u64), |_| unreachable!());
    assert_eq!(range.unwrap_err(), ("RangeError", "Invalid time value".to_string()));
    let mut script = String::new();
    let r = schedule(EXE, r"C:\d", &[], &json!(AT), |c| {
        script = c.script.clone();
        answer(&saved())
    });
    assert_eq!(r.unwrap(), saved());
    assert!(script.contains("StartBoundary = '2026-11-22T16:05:21.000Z'") && script.contains(&ps_text(ARGS)));
}

#[test]
fn read_back_verification() {
    let bad = [
        ("enabled", json!(false)),
        ("missed", json!(0)),
        ("end", json!("")),
        ("logon", json!("9")),
        ("exe", json!("x")),
        ("args", json!("")),
        ("next", json!(1_795_363_522_001_u64)),
        ("next", Value::Null),
    ];
    for (k, v) in bad {
        let mut s = saved();
        s[k] = v;
        assert_eq!(message(run(json!(AT), answer(&s))), NOT_VERIFIED, "{k}");
    }
    let mut no_next = saved();
    no_next.as_object_mut().map(|o| o.remove("next"));
    assert!(run(json!(AT), answer(&no_next)).is_ok(), "NaN > 1000 is false in JS");
    assert_eq!(run(json!(AT), answer(&Value::Null)).unwrap_err().0, "TypeError");
    assert_eq!(message(run(json!(AT), answer(&json!([1])))), NOT_VERIFIED);
}

fn out(error: Option<&str>, stdout: Option<&str>, stderr: Option<&str>) -> Value {
    json!({"error": error.map(|m| json!({"message": m})), "stdout": stdout, "stderr": stderr})
}

#[test]
fn error_mapping() {
    let msg = |a: Value| message(read_output(&a));
    assert_eq!(msg(out(Some("m"), Some(""), Some("\u{feff} a\r\nb"))), "a");
    assert_eq!(msg(out(Some("m"), None, Some("  \n"))), "");
    assert_eq!(msg(out(Some("m"), None, Some(""))), "m");
    assert_eq!(msg(out(Some("m\nx"), None, None)), "m");
    assert_eq!(msg(out(Some("m"), None, Some("\u{85}a\u{85}\nb"))), "\u{85}a\u{85}");
    assert_eq!(msg(out(None, Some("oops"), None)), NOT_CONFIRMED);
    assert_eq!(msg(out(None, None, None)), NOT_CONFIRMED);
    assert_eq!(read_output(&out(None, Some("\u{a0}[1]\n"), None)).unwrap(), json!([1]));
}

#[test]
fn json_edge_cases_of_js_parse() {
    assert!(read_output(&out(None, Some("1e999"), None)).is_ok());
    assert_eq!(read_output(&out(None, Some(r#""\ud800""#), None)).unwrap(), json!("\u{fffd}"));
    assert_eq!(read_output(&out(None, Some(r#""\ud83d\ude00""#), None)).unwrap(), json!("😀"));
}

#[test]
fn js_number_coercion() {
    let good = [("", 0.0), (" 12 ", 12.0), ("0x10", 16.0), ("0b11", 3.0), ("0o7", 7.0), ("1e3", 1000.0), (".5", 0.5), ("5.", 5.0)];
    for (s, n) in good {
        assert_eq!(string_to_number(s), n, "{s:?}");
    }
    assert_eq!(string_to_number("-Infinity"), f64::NEG_INFINITY);
    for s in ["abc", "1_0", "-0x1", ".", "1e", "+-1", "0x", "inf", "NaN", "+"] {
        assert!(string_to_number(s).is_nan(), "{s:?}");
    }
    assert_eq!(to_number(&json!([[5]])), 5.0);
    assert!(to_number(&json!({})).is_nan());
}

#[test]
fn cancel_script_and_exports() {
    let mut seen = None;
    assert_eq!(
        cancel("d", |c| {
            seen = Some(c.clone());
            out(None, Some("null"), None)
        })
        .unwrap(),
        Value::Null
    );
    let script = seen.map(|c| c.script).unwrap_or_default();
    assert!(script.starts_with("try { $ErrorActionPreference = 'Stop'") && script.ends_with("exit 1 }"));
    assert!(script.contains(&format!("$name = '{}'", task_name_of("d"))));
    assert!(cancel_call(json!("d"), json!(5)).is_err());
    assert_eq!(ps(json!(1)).unwrap_err(), "oracle: value must be a string");
}
