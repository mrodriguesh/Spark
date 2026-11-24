//! The Rust port of `scheduler.js`: pure functions for the PowerShell argv and script text, the read-back
//! verification and the error mapping. No process is spawned here: the caller passes an `exec` that plays
//! `execFile`. Script texts are byte-identical to the original's template literals.

#![forbid(unsafe_code)]

use base64::{Engine, engine::general_purpose::STANDARD};
use regex::{Captures, Regex};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const CONNECT_A: &str = "$ErrorActionPreference = 'Stop'\n    [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)\n    $service = New-Object -ComObject 'Schedule.Service'; $service.Connect()\n    $root = $service.GetFolder('\\'); $name = ";
const CONNECT_B: &str = "\n  ";
const CMD_A: &str = "try { ";
const CMD_B: &str = " } catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }";
const SCHED_A: &str = "\n        $user = [Security.Principal.WindowsIdentity]::GetCurrent().Name\n        $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value\n        $security = 'D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;' + $sid + ')'\n        $def = $service.NewTask(0)\n        $def.RegistrationInfo.Description = 'Reabre o Spark minimizado para tentar novamente após o limite de curtidas.'\n        $def.Principal.UserId = $user; $def.Principal.LogonType = 3; $def.Principal.RunLevel = 0\n        $def.Settings.Enabled = $true; $def.Settings.StartWhenAvailable = $true\n        $def.Settings.DisallowStartIfOnBatteries = $false; $def.Settings.StopIfGoingOnBatteries = $false\n        $def.Settings.ExecutionTimeLimit = 'PT0S'; $def.Settings.MultipleInstances = 2\n        $trigger = $def.Triggers.Create(1); $trigger.StartBoundary = ";
const SCHED_B: &str = "\n        # O Windows só recupera horários perdidos de um gatilho pontual com data final.\n        $trigger.EndBoundary = '9999-12-31T00:00:00Z'\n        $logon = $def.Triggers.Create(9); $logon.UserId = $sid; $logon.Delay = 'PT15S'\n        $action = $def.Actions.Create(0); $action.Path = ";
const SCHED_C: &str = "; $action.Arguments = ";
const SCHED_D: &str = "\n        $null = $root.RegisterTaskDefinition($name, $def, 6, $user, $null, 3, $security)\n        $saved = $root.GetTask($name)\n        @{ enabled = $saved.Enabled; exe = $saved.Definition.Actions.Item(1).Path;\n           args = $saved.Definition.Actions.Item(1).Arguments;\n           missed = $saved.Definition.Settings.StartWhenAvailable;\n           end = $saved.Definition.Triggers.Item(1).EndBoundary;\n           logon = $saved.Definition.Triggers.Item(2).Type;\n           next = ([DateTimeOffset]$saved.NextRunTime).ToUnixTimeMilliseconds() } | ConvertTo-Json -Compress\n      ";
const CANCEL_A: &str =
    "\n      foreach ($task in $root.GetTasks(1)) { if ($task.Name -eq $name) { $root.DeleteTask($name, 0); break } }\n      'null'\n    ";

const BAD_TIME: &str = "Horário inválido para a próxima tentativa";
const NOT_CONFIRMED: &str = "O Windows não confirmou o agendamento";
const NOT_VERIFIED: &str = "A próxima abertura não foi confirmada pelo Windows";
const BAD_ANSWER: &str = "oracle: answer must be {error: null | {message: string}, stdout: string | null, stderr: string | null}";

pub(crate) const FILE: &str = "powershell.exe";
pub(crate) const TIMEOUT_MS: u64 = 20000;

/// A JS exception as (constructor name, message).
pub(crate) type JsErr = (&'static str, String);

fn fail<T>(name: &'static str, message: &str) -> Result<T, JsErr> {
    Err((name, message.to_string()))
}

/// One `execFile('powershell.exe', args, { windowsHide: true, timeout: 20000 })` call.
#[derive(Clone)]
pub(crate) struct Call {
    pub(crate) args: [String; 4],
    pub(crate) script: String,
}

/// `ps` (`scheduler.js:4`): a PowerShell single-quoted literal.
pub(crate) fn ps_text(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// `quote` (`scheduler.js:5`): always-quoted argv element, backslashes before `"` and at the end doubled.
pub(crate) fn quote_text(value: &str) -> String {
    let (mut out, mut slashes) = (String::from('"'), 0);
    for c in value.chars() {
        let extra = if c == '"' { slashes + 1 } else { 0 };
        match c {
            '\\' => slashes += 1,
            _ => {
                out += &"\\".repeat(slashes + extra);
                out.push(c);
                slashes = 0;
            }
        }
    }
    out + &"\\".repeat(slashes * 2) + "\""
}

/// The task name: `Spark - Intervalo - ` + 16 hex of sha256(utf8(lowercase(data))).
pub(crate) fn task_name_of(data: &str) -> String {
    let hex: String = Sha256::digest(data.to_lowercase().as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("Spark - Intervalo - {hex}")
}

fn call(data: &str, script: &str) -> Call {
    let command = format!("{CMD_A}{CONNECT_A}{}{CONNECT_B}{script}{CMD_B}", ps_text(&task_name_of(data)));
    let utf16: Vec<u8> = command.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let [a, b, c] = ["-NoProfile", "-NonInteractive", "-EncodedCommand"].map(String::from);
    Call { args: [a, b, c, STANDARD.encode(utf16)], script: command }
}

/// `new Date(ms).toISOString()` for `0 <= ms <= 8.64e15` (civil-from-days).
fn iso_string(ms: i64) -> String {
    let (z, rest) = (ms.div_euclid(86_400_000) + 719_468, ms.rem_euclid(86_400_000));
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let (day, month) = (doy - (153 * mp + 2) / 5 + 1, if mp < 10 { mp + 3 } else { mp - 9 });
    let year = yoe + era * 400 + i64::from(month <= 2);
    let year = if year > 9999 { format!("+{year:06}") } else { format!("{year:04}") };
    let (h, m, s, milli) = (rest / 3_600_000, rest / 60_000 % 60, rest / 1000 % 60, rest % 1000);
    format!("{year}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}.{milli:03}Z")
}

/// `String.prototype.trim` whitespace: White_Space minus U+0085, plus U+FEFF.
fn is_js_space(c: char) -> bool {
    matches!(c, '\t'..='\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

/// Text `JSON.parse` accepts but serde_json refuses: lone surrogate escapes become U+FFFD and numbers beyond f64
/// become +-1e308 (JS: Infinity). A retry after serde_json failed; it also rewrites such text inside strings.
fn lenient(text: &str) -> String {
    let (Ok(lone), Ok(num)) =
        (Regex::new(r"(?i)\\ud[89ab][0-9a-f]{2}\\ud[c-f][0-9a-f]{2}|\\ud[89a-f][0-9a-f]{2}"), Regex::new(r"-?\d+(\.\d+)?[eE][+-]?\d+"))
    else {
        return text.into();
    };
    let fixed = lone.replace_all(text, |m: &Captures| if m[0].len() == 12 { m[0].to_string() } else { "\\ufffd".into() });
    let inf = |m: &Captures| match m[0].parse::<f64>() {
        Ok(f) if f.is_infinite() => (if f < 0.0 { "-1e308" } else { "1e308" }).into(),
        _ => m[0].to_string(),
    };
    num.replace_all(&fixed, inf).into_owned()
}

/// The callback of `run`: `answer` is `{error, stdout, stderr}`. Error -> first stderr (else message) line.
pub(crate) fn read_output(answer: &Value) -> Result<Value, JsErr> {
    if answer["error"].is_object() {
        let stderr = answer["stderr"].as_str().filter(|s| !s.is_empty());
        let text = stderr.or(answer["error"]["message"].as_str()).unwrap_or_default().trim_matches(is_js_space);
        let line = text.split('\n').next().unwrap_or_default();
        return fail("Error", line.strip_suffix('\r').unwrap_or(line));
    }
    let Some(text) = answer["stdout"].as_str().map(|s| s.trim_matches(is_js_space)) else { return fail("Error", NOT_CONFIRMED) };
    serde_json::from_str(text).or_else(|_| serde_json::from_str(&lenient(text))).or_else(|_| fail("Error", NOT_CONFIRMED))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null | Value::Bool(false) => false,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// JS `StringToNumber`.
fn string_to_number(s: &str) -> f64 {
    let s = s.trim_matches(is_js_space);
    let (digits, radix) = match s.get(..2) {
        Some("0x" | "0X") => (&s[2..], 16),
        Some("0b" | "0B") => (&s[2..], 2),
        Some("0o" | "0O") => (&s[2..], 8),
        _ => ("", 10),
    };
    let unsigned = s.strip_prefix(['+', '-']).unwrap_or(s);
    if radix != 10 {
        let fold = |n: f64, c: char| c.to_digit(radix).map(|d| n * f64::from(radix) + f64::from(d));
        return if digits.is_empty() { f64::NAN } else { digits.chars().try_fold(0.0, fold).unwrap_or(f64::NAN) };
    }
    match unsigned {
        "" if s.is_empty() => 0.0,
        "Infinity" => {
            if s.starts_with('-') {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            }
        }
        _ if unsigned.chars().all(|c| "0123456789.eE+-".contains(c)) => s.parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// JS `ToNumber` of a parsed JSON value (arrays go through their `join(',')` string).
fn to_number(v: &Value) -> f64 {
    fn text(v: &Value) -> String {
        match v {
            Value::Null => String::new(),
            Value::Array(a) => a.iter().map(text).collect::<Vec<_>>().join(","),
            Value::Object(_) => "[object Object]".into(),
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
    match v {
        Value::Null => 0.0,
        Value::Bool(b) => f64::from(u8::from(*b)),
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        other => string_to_number(&text(other)),
    }
}

/// `schedule(at)`: validate, build the call, hand it to `exec` (which plays `execFile` and returns
/// `{error, stdout, stderr}`), then check the read-back.
pub(crate) fn schedule(exe: &str, data: &str, launch: &[String], at: &Value, exec: impl FnOnce(&Call) -> Value) -> Result<Value, JsErr> {
    let at = at.as_f64().filter(|f| f.fract() == 0.0 && *f > 0.0 && *f <= 9_007_199_254_740_991.0);
    let Some(at) = at else { return fail("Error", BAD_TIME) };
    let run_at = (at / 1000.0).ceil() * 1000.0;
    if run_at > 8.64e15 {
        return fail("RangeError", "Invalid time value");
    }
    let all = launch.iter().cloned().chain([format!("--user-data-dir={data}"), format!("--spark-retry-at={}", at as i64)]);
    let args = all.map(|a| quote_text(&a)).collect::<Vec<_>>().join(" ");
    let script = format!("{SCHED_A}{}{SCHED_B}{}{SCHED_C}{}{SCHED_D}", ps_text(&iso_string(run_at as i64)), ps_text(exe), ps_text(&args));
    let saved = read_output(&exec(&call(data, &script)))?;
    if saved.is_null() {
        return fail("TypeError", "Cannot read properties of null (reading 'enabled')");
    }
    let same = |k: &str, want: &str| saved[k].as_str() == Some(want);
    let next = saved.get("next").map_or(f64::NAN, to_number);
    let ok = ["enabled", "missed", "end"].iter().all(|k| truthy(&saved[*k])) && saved["logon"].as_f64() == Some(9.0);
    if !(ok && same("exe", exe) && same("args", &args)) || (next - run_at).abs() > 1000.0 {
        return fail("Error", NOT_VERIFIED);
    }
    Ok(saved)
}

/// `cancel()`: same transport, no verification; `exec` as in `schedule`.
pub(crate) fn cancel(data: &str, exec: impl FnOnce(&Call) -> Value) -> Result<Value, JsErr> {
    read_output(&exec(&call(data, CANCEL_A)))
}

fn text_arg(name: &str, v: &Value) -> Result<String, String> {
    v.as_str().map(String::from).ok_or_else(|| format!("oracle: {name} must be a string"))
}

fn check_answer(a: &Value) -> Result<Value, String> {
    let text = |k: &str| a.get(k).is_none_or(|v| v.is_null() || v.is_string());
    let error = a.get("error").is_none_or(|e| e.is_null() || e.get("message").is_some_and(Value::is_string));
    if a.is_object() && error && text("stdout") && text("stderr") { Ok(a.clone()) } else { Err(BAD_ANSWER.into()) }
}

/// The oracle's report of one operation: what the fake `execFile` saw and what the promise settled to.
fn report(seen: Option<Call>, name: &str, outcome: Result<Value, JsErr>) -> Value {
    let (ok, result, error) = match outcome {
        Ok(v) => (true, v, Value::Null),
        Err((name, message)) => (false, Value::Null, json!({ "name": name, "message": message })),
    };
    let c = seen.as_ref();
    json!({
        "name": name, "called": c.is_some(), "file": c.map(|_| FILE), "args": c.map(|c| &c.args),
        "windowsHide": c.map(|_| true), "timeout": c.map(|_| TIMEOUT_MS), "script": c.map(|c| &c.script),
        "ok": ok, "result": result, "error": error, "calls": u8::from(c.is_some()),
    })
}

/// Port of `ps`.
pub fn ps(value: Value) -> Result<Value, String> {
    Ok(json!(ps_text(&text_arg("value", &value)?)))
}

/// Port of `quote`.
pub fn quote(value: Value) -> Result<Value, String> {
    Ok(json!(quote_text(&text_arg("value", &value)?)))
}

/// Port of `taskName`.
pub fn task_name(data: Value) -> Result<Value, String> {
    Ok(json!(task_name_of(&text_arg("data", &data)?)))
}

/// Port of `scheduleCall`: `schedule(at)` with an injected `execFile` answer.
pub fn schedule_call(exe: Value, data: Value, launch_args: Value, at: Value, answer: Value) -> Result<Value, String> {
    let (exe, data) = (text_arg("exe", &exe)?, text_arg("data", &data)?);
    let launch: Option<Vec<String>> = launch_args.as_array().and_then(|a| a.iter().map(|v| v.as_str().map(String::from)).collect());
    let launch = launch.ok_or("oracle: launchArgs must be an array of strings")?;
    let answer = check_answer(&answer)?;
    let mut seen = None;
    let outcome = schedule(&exe, &data, &launch, &at, |c| {
        seen = Some(c.clone());
        answer
    });
    Ok(report(seen, &task_name_of(&data), outcome))
}

/// Port of `cancelCall`: `cancel()` with an injected `execFile` answer.
pub fn cancel_call(data: Value, answer: Value) -> Result<Value, String> {
    let data = text_arg("data", &data)?;
    let answer = check_answer(&answer)?;
    let mut seen = None;
    let outcome = cancel(&data, |c| {
        seen = Some(c.clone());
        answer
    });
    Ok(report(seen, &task_name_of(&data), outcome))
}

#[cfg(test)]
#[path = "../tests_port.rs"]
mod tests;
