//! The Rust port of the spark.json store (SPEC S-02.1-S-02.5): engine.js `writeJson`/`readJson`
//! (engine.js:77-111) and the startup load, recovery and migration of main.js:56-86.
//! The `*_scenario` functions mirror the original, which runs the originals.

#![forbid(unsafe_code)]

use serde_json::{Map, Value};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, UNIX_EPOCH};

/// `db`: the persisted settings, a JSON object in insertion order.
pub type Db = Map<String, Value>;
pub const FILE_NAME: &str = "spark.json";
pub const RETRY_MINUTES: [f64; 5] = [15.0, 30.0, 60.0, 120.0, 360.0];
/// Keys of removed features, dropped on load (main.js:76).
const OLD: [&str; 9] = [
    "recheckMin",
    "waitFrom",
    "waitWhy",
    "admirerOn",
    "cycles",
    "admirers",
    "driver",
    "geoLat",
    "geoLon",
];
/// `Date.parse('9999-12-30T00:00:00Z')`.
const MAX_WAIT: f64 = 253_402_128_000_000.0;
const INVALID: &str = "Error: Arquivo de configurações inválido";
/// serde_json refuses a lone surrogate escape that JSON.parse accepts: it rides as U+10F800 + (unit - 0xD800).
/// shortcut: a literal U+10F800..U+10FFFF in spark.json is re-saved as a surrogate escape; only hand edits hold either.
const LONE: u32 = 0x10F800;

/// main.js:58 defaults, in their order.
pub fn defaults() -> Db {
    let d = [
        ("enabled", Value::Bool(true)),
        ("skipFirst", Value::Bool(true)),
        ("autostart", Value::Bool(true)),
        ("day", Value::from("")),
        ("likesToday", Value::from(0)),
        ("likesTotal", Value::from(0)),
        ("matches", Value::from(0)),
        ("startMinimized", Value::Bool(false)),
        ("retryMin", Value::from(15)),
        ("likesWaitFrom", Value::from(0)),
    ];
    d.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// `JSON.parse`; Err is `SyntaxError: <serde_json's message>`.
/// shortcut: serde_json stops at 128 nested levels, V8 does not; only a hand-edited file nests that deep.
pub fn parse(text: &str) -> Result<Value, String> {
    serde_json::from_str(&mark_lone_surrogates(text)).map_err(|e| format!("SyntaxError: {e}"))
}

fn hex4(b: &[u8], at: usize) -> Option<u32> {
    u32::from_str_radix(std::str::from_utf8(b.get(at..at + 4)?).ok()?, 16).ok()
}

fn mark_lone_surrogates(text: &str) -> std::borrow::Cow<'_, str> {
    let b = text.as_bytes();
    let (mut out, mut last, mut i) = (String::new(), 0, 0);
    while i < b.len() {
        if b[i] != b'\\' {
            i += 1;
        } else if b.get(i + 1) != Some(&b'u') {
            i += 2; // `\\`, `\"`, ...: the next byte is never the start of an escape
        } else {
            let pair = |n: u32| {
                (0xD800..0xDC00).contains(&n)
                    && b.get(i + 6..i + 8) == Some(b"\\u")
                    && hex4(b, i + 8).is_some_and(|m| (0xDC00..0xE000).contains(&m))
            };
            match hex4(b, i + 2) {
                Some(n) if pair(n) => i += 12,
                Some(n) if (0xD800..0xE000).contains(&n) => {
                    out.push_str(&text[last..i]);
                    out.extend(char::from_u32(LONE + n - 0xD800));
                    i += 6;
                    last = i;
                }
                _ => i += 2,
            }
        }
    }
    if last == 0 {
        return text.into();
    }
    out.push_str(&text[last..]);
    out.into()
}

/// `JSON.stringify` of a JSON value: JS key order (array indexes first), numbers as Number#toString,
/// non-finite numbers as `null`, lone surrogates as lowercase `\udxxx`.
pub fn stringify(v: &Value) -> String {
    let mut out = String::new();
    write_value(v, &mut out);
    out
}

fn write_value(v: &Value, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match n.as_str().parse::<f64>() {
            Ok(f) if f.is_finite() => out.push_str(&crate::js::number_to_string(f)),
            _ => out.push_str("null"), // 1e400 parses to Infinity
        },
        Value::String(s) => quote(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(x, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            let mut entries: Vec<_> = m.iter().collect();
            entries.sort_by_key(|(k, _)| array_index(k).map_or((1, 0), |i| (0, i))); // stable
            out.push('{');
            for (i, (k, x)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                quote(k, out);
                out.push(':');
                write_value(x, out);
            }
            out.push('}');
        }
    }
}

/// A key JS objects list first, in ascending order: a canonical integer below 2^32 - 1.
fn array_index(k: &str) -> Option<u32> {
    let i: u32 = k.parse().ok()?;
    (i < u32::MAX && i.to_string() == k).then_some(i)
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\0'..='\u{1f}' => out.push_str(&format!("\\u{:04x}", c as u32)),
            '\u{10F800}'..='\u{10FFFF}' => {
                out.push_str(&format!("\\u{:04x}", c as u32 - LONE + 0xD800))
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn with_suffix(file: &Path, suffix: &str) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(suffix);
    s.into()
}

fn io_err(e: std::io::Error) -> String {
    format!("Error: {e}")
}

/// `fs.readFileSync(name, 'utf8')`: invalid UTF-8 becomes U+FFFD.
fn read_text(path: &Path) -> std::io::Result<String> {
    fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// `fs.writeFileSync(path, data, { flush: true })`.
fn write_flush(path: &Path, data: &str) -> Result<(), String> {
    let mut f = fs::File::create(path).map_err(io_err)?;
    f.write_all(data.as_bytes())
        .and_then(|()| f.sync_all())
        .map_err(io_err)
}

/// `writeJson`: the new text to `.tmp` (flushed), the old text to `.bak` when it parses, then `.tmp` over `file`.
pub fn write_json(file: &Path, obj: &Value) -> Result<(), String> {
    write_json_with(file, obj, |from, to| fs::rename(from, to).map_err(io_err))
}

fn write_json_with(
    file: &Path,
    obj: &Value,
    rename: impl FnOnce(&Path, &Path) -> Result<(), String>,
) -> Result<(), String> {
    let previous = match read_text(file) {
        Ok(t) => parse(&t).is_ok().then_some(t),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(io_err(e)),
    };
    let tmp = with_suffix(file, ".tmp");
    write_flush(&tmp, &stringify(obj))?;
    if let Some(p) = previous {
        write_flush(&with_suffix(file, ".bak"), &p)?;
    }
    rename(&tmp, file)
}

/// Node's `mtimeMs`: `sec * 1e3 + nsec / 1e6` in doubles. shortcut: a pre-1970 mtime reads as 0.
fn mtime_ms(m: &fs::Metadata) -> f64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |d| {
            d.as_secs() as f64 * 1e3 + f64::from(d.subsec_nanos()) / 1e6
        })
}

/// `readJson`: `.tmp` and `file` newest first, then `.bak`; the first JSON object wins.
/// `Ok(None)` when none exists; otherwise the last parse or shape error.
pub fn read_json(file: &Path) -> Result<Option<(Db, PathBuf)>, String> {
    let mut primary = Vec::new();
    for name in [with_suffix(file, ".tmp"), file.to_path_buf()] {
        match fs::metadata(&name) {
            Ok(m) => primary.push((mtime_ms(&m), name)),
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(io_err(e)),
        }
    }
    primary.sort_by(|a, b| b.0.total_cmp(&a.0)); // stable: a tie keeps `.tmp` first, as V8's sort
    let mut invalid = None;
    for name in primary
        .into_iter()
        .map(|(_, n)| n)
        .chain([with_suffix(file, ".bak")])
    {
        let text = match read_text(&name) {
            Ok(t) => t,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(io_err(e)),
        };
        match parse(&text) {
            Ok(Value::Object(data)) => return Ok(Some((data, name))),
            Ok(_) => invalid = Some(INVALID.to_string()),
            Err(e) => invalid = Some(e),
        }
    }
    invalid.map_or(Ok(None), Err)
}

/// `save()`: never fails; prints the error as `console.error('save', e)` does.
pub fn save(file: &Path, db: &Db) -> bool {
    match write_json(file, &Value::Object(db.clone())) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("save {e}");
            false
        }
    }
}

/// What main.js's top level holds after loading: `db`, `loadError`, `recoveredFrom`.
pub struct Loaded {
    pub db: Db,
    pub load_error: Option<String>,
    pub recovered_from: Option<String>,
}

/// main.js:58-82: defaults merged with the saved object, recovery copies, then the migration.
pub fn load_db(file: &Path) -> Loaded {
    let (mut db, mut load_error, mut recovered_from) = (defaults(), None, None);
    match read_json(file) {
        Ok(found) => {
            if let Some((data, source)) = found {
                db.extend(data);
                if source != file {
                    recovered_from = source.file_name().map(|n| n.to_string_lossy().into_owned());
                    if source == with_suffix(file, ".bak") {
                        db.insert("enabled".into(), Value::Bool(false)); // the copy may predate a pause
                    }
                    let _ = fs::copy(file, with_suffix(file, ".antes-recuperacao")); // best effort
                    save(file, &db);
                }
            }
        }
        Err(e) => {
            load_error = Some(e);
            db.insert("enabled".into(), Value::Bool(false));
            let _ = fs::copy(file, with_suffix(file, ".corrompido")); // best effort
        }
    }
    let num = |k: &str| match db.get(k) {
        Some(Value::Number(n)) => n.as_str().parse::<f64>().ok(),
        _ => None,
    };
    let retry_ok = num("retryMin").is_some_and(|r| RETRY_MINUTES.contains(&r));
    // Number.isSafeInteger and 0 <= w <= MAX_WAIT (which is below 2^53)
    let valid_wait =
        num("likesWaitFrom").is_some_and(|w| w.fract() == 0.0 && (0.0..=MAX_WAIT).contains(&w));
    if !retry_ok {
        db.insert("retryMin".into(), Value::from(15));
    }
    if !valid_wait {
        db.insert("likesWaitFrom".into(), Value::from(0));
    }
    if !retry_ok || !valid_wait || OLD.iter().any(|k| db.contains_key(*k)) {
        for k in OLD {
            db.shift_remove(k);
        }
        save(file, &db);
    }
    Loaded {
        db,
        load_error,
        recovered_from,
    }
}

// ---- the oracle's scenarios (kept from the recorded original) ------------------------------------------

const NAMES: [(&str, &str); 5] = [
    ("main", "spark.json"),
    ("tmp", "spark.json.tmp"),
    ("bak", "spark.json.bak"),
    ("antes", "spark.json.antes-recuperacao"),
    ("corrompido", "spark.json.corrompido"),
];

fn temp_dir() -> Result<PathBuf, String> {
    static N: AtomicU32 = AtomicU32::new(0);
    loop {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("spark-oracle-{}-{n}", std::process::id()));
        match fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io_err(e)),
        }
    }
}

/// `inDir`: writes the files in a fresh temp dir, sets the mtimes, runs, adds `after`, deletes the dir.
fn scenario(files: &Value, newer: &Value, run: impl FnOnce(&Path) -> Db) -> Result<Value, String> {
    let dir = temp_dir()?;
    let result = (|| -> Result<Value, String> {
        for (k, name) in &NAMES[..3] {
            if let Some(Value::String(s)) = files.get(k) {
                fs::write(dir.join(name), s).map_err(io_err)?;
            }
        }
        for (k, name) in &NAMES[..2] {
            let t = UNIX_EPOCH
                + Duration::from_secs(
                    1_700_000_000 + if newer.as_str() == Some(*k) { 10 } else { 0 },
                );
            if let Ok(f) = fs::File::options().write(true).open(dir.join(name)) {
                f.set_modified(t).map_err(io_err)?;
            }
        }
        let mut out = run(&dir.join(FILE_NAME));
        let after = NAMES.iter().map(|(k, name)| {
            (
                k.to_string(),
                read_text(&dir.join(name)).map_or(Value::Null, Value::String),
            )
        });
        out.insert("after".into(), Value::Object(after.collect()));
        Ok(Value::Object(out))
    })();
    let _ = fs::remove_dir_all(&dir);
    result
}

/// The oracle's `describe`: V8's SyntaxError text is not compared, only its name.
fn describe(e: String) -> Value {
    Value::String(if e.starts_with("SyntaxError") {
        "SyntaxError".into()
    } else {
        e
    })
}

fn entries<const N: usize>(e: [(&str, Value); N]) -> Db {
    e.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// JS truthiness of a JSON argument.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// Port of `writeScenario` (`store.js:39`).
pub fn write_scenario(files: Value, obj: Value, fail_rename: Value) -> Result<Value, String> {
    scenario(&files, &Value::Null, |file| {
        let r = if truthy(&fail_rename) {
            write_json_with(file, &obj, |_, _| Err("Error: interrupção simulada".into()))
        } else {
            write_json(file, &obj)
        };
        match r {
            Ok(()) => entries([("ok", Value::Bool(true))]),
            Err(e) => entries([("error", describe(e))]),
        }
    })
}

/// Port of `readScenario` (`store.js:47`).
pub fn read_scenario(files: Value, newer: Value) -> Result<Value, String> {
    scenario(&files, &newer, |file| match read_json(file) {
        Ok(None) => entries([("data", Value::from("null")), ("source", Value::Null)]),
        Ok(Some((data, source))) => entries([
            ("data", Value::String(stringify(&Value::Object(data)))),
            (
                "source",
                source.file_name().map_or(Value::Null, |n| {
                    Value::from(n.to_string_lossy().into_owned())
                }),
            ),
        ]),
        Err(e) => entries([("error", describe(e))]),
    })
}

/// Port of `loadScenario` (`store.js:58`).
pub fn load_scenario(files: Value, newer: Value) -> Result<Value, String> {
    scenario(&files, &newer, |file| {
        let l = load_db(file);
        entries([
            ("db", Value::String(stringify(&Value::Object(l.db)))),
            ("loadError", l.load_error.map_or(Value::Null, describe)),
            (
                "recoveredFrom",
                l.recovered_from.map_or(Value::Null, Value::String),
            ),
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(text: &str) -> Value {
        parse(text).unwrap()
    }

    /// test/storage.js, on real temp files.
    #[test]
    fn storage() {
        let root = temp_dir().unwrap();
        let file = root.join(FILE_NAME);
        let (tmp, bak) = (with_suffix(&file, ".tmp"), with_suffix(&file, ".bak"));
        let previous = obj(
            r#"{"enabled":true,"likesTotal":2868,"matches":5,"retryMin":360,"likesWaitFrom":123}"#,
        );
        let paused = obj(
            r#"{"enabled":false,"likesTotal":2868,"matches":5,"retryMin":360,"likesWaitFrom":123}"#,
        );
        let data = |r: Option<(Db, PathBuf)>| r.map(|(d, s)| (Value::Object(d), s));
        let read_back = |p: &Path| obj(&fs::read_to_string(p).unwrap());

        assert_eq!(read_json(&file), Ok(None));
        write_json(&file, &previous).unwrap();
        write_json(&file, &paused).unwrap();
        assert_eq!(data(read_json(&file).unwrap()).unwrap().0, paused);
        assert_eq!(read_back(&bak), previous);
        assert!(!tmp.exists());
        // interrupted after writing the temporary, before replacing the main file
        let e = write_json_with(&file, &previous, |_, _| {
            Err("Error: interrupção simulada".into())
        });
        assert!(e.unwrap_err().contains("interrupção simulada"));
        assert_eq!(
            data(read_json(&file).unwrap()),
            Some((previous.clone(), tmp.clone()))
        );
        write_json(&file, &previous).unwrap();
        fs::write(&file, r#"{"enabled":"#).unwrap();
        assert_eq!(
            data(read_json(&file).unwrap()).unwrap().0,
            paused,
            "corrupt main recovers the last valid copy"
        );
        fs::write(&tmp, stringify(&paused)).unwrap();
        assert_eq!(
            data(read_json(&file).unwrap()),
            Some((paused.clone(), tmp.clone()))
        );
        write_json(&file, &paused).unwrap();
        assert_eq!(
            data(read_json(&file).unwrap()).unwrap().0,
            paused,
            "a write never replaces the valid copy with corrupt JSON"
        );
        fs::write(&tmp, "{").unwrap();
        fs::write(&file, "null").unwrap();
        fs::write(&bak, "[]").unwrap();
        assert!(
            read_json(&file)
                .unwrap_err()
                .contains("configurações inválido")
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stringify_like_js() {
        let t = r#"{"b":1e21,"2":-0,"1":1e400,"4294967295":"\ud800A😀","01":"\u0001\u007f"}"#;
        let js = "{\"1\":null,\"2\":0,\"b\":1e+21,\"4294967295\":\"\\ud800A😀\",\"01\":\"\\u0001\u{7f}\"}";
        assert_eq!(stringify(&obj(t)), js);
        assert!(
            parse("\u{feff}{}").is_err()
                && parse(r#"{"a":"\x"}"#).is_err()
                && parse(r#"["\\ud800"]"#).is_ok()
        );
    }
}
