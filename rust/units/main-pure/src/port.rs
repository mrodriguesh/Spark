//! The Rust port of `main.js`'s pure helpers `allowed` (main.js:33-38) and `NET_DOWN` (main.js:143).
//!
//! `allowed` uses WHATWG `new URL`: the `url` crate parses the same grammar (it strips leading/trailing C0 controls
//! and spaces and inner tab/CR/LF, flips `\` to `/` for special schemes, lowercases and IDNA-maps hosts, decodes
//! percent-escapes in hosts), so no special-casing of the allowlist logic is needed.

#![forbid(unsafe_code)]

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

/// Only Tinder and its login providers may open in the embedded browser (`main.js:31`).
const ALLOWED: [&str; 7] = [
    "tinder.com",
    "gotinder.com",
    "google.com",
    "gstatic.com",
    "facebook.com",
    "apple.com",
    "arkoselabs.com",
];

/// `NET_DOWN` (`main.js:143`): case-sensitive, `$` only at the very end of the text.
static NET_DOWN: LazyLock<Regex> = LazyLock::new(|| {
    // INFALLIBLE: constant pattern, exercised by every test.
    Regex::new(r"ERR_(INTERNET_DISCONNECTED|NAME_NOT_RESOLVED|NAME_RESOLUTION_FAILED|ADDRESS_UNREACHABLE|NETWORK_CHANGED|TIMED_OUT|CONNECTION_(REFUSED|RESET|TIMED_OUT|CLOSED|FAILED)|PROXY_CONNECTION_FAILED)$")
        .expect("valid pattern")
});

/// JS `String(value)` for a JSON value (what `new URL(x)` / `RegExp.test(x)` do with a non-string); the record
/// codec's `{"$utf16": [...]}` (lone surrogates) becomes U+FFFD, which is also what `USVString` conversion does.
fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|e| {
                if e.is_null() || crate::js::is_undefined(e) {
                    String::new()
                } else {
                    js_string(e)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(o) if o.len() == 1 && o.contains_key("$utf16") => char::decode_utf16(
            o["$utf16"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .map(|u| u as u16),
        )
        .map(|c| c.unwrap_or('\u{FFFD}'))
        .collect(),
        Value::Object(o) if o.len() == 1 && o.contains_key("$num") => {
            o["$num"].as_str().unwrap_or("NaN").replace("-0", "0")
        }
        Value::Object(_) if crate::js::is_undefined(v) => "undefined".into(),
        Value::Object(_) => "[object Object]".into(),
        other => other.to_string(),
    }
}

/// Plain Rust entry points for the app (not bridged).
pub mod plain {
    use super::{ALLOWED, NET_DOWN};

    /// `url` with every ASCII-case-insensitive `xn--` turned into `xn-a` (same length, still a valid label).
    fn defang(url: &str) -> String {
        let mut out = String::with_capacity(url.len());
        let mut rest = url;
        while let Some(i) = rest.to_ascii_lowercase().find("xn--") {
            out.push_str(&rest[..i + 3]);
            out.push('a');
            rest = &rest[i + 4..];
        }
        out.push_str(rest);
        out
    }

    /// Port of `allowed` (`main.js:33`) on plain text; a string that `new URL` rejects is `false`.
    pub fn allowed_str(url: &str) -> bool {
        let parsed = match url::Url::parse(url) {
            // DEVIATION of the `url` crate from Node 26 (ada): ada does not validate punycode in an all-ASCII host
            // (`https://xn--.tinder.com` parses, hostname `xn--.tinder.com`), the crate's IDNA step rejects it.
            // Only for an all-ASCII input that failed with an IDNA error: parse again with `xn--` defanged, which
            // keeps every other part of the host (the allowlist suffix has no `xn--`).
            Err(url::ParseError::IdnaError) if url.is_ascii() => url::Url::parse(&defang(url)),
            other => other,
        };
        let Ok(u) = parsed else {
            return false;
        };
        u.scheme() == "https"
            && u.host_str().is_some_and(|h| {
                ALLOWED
                    .iter()
                    .any(|d| h == *d || h.strip_suffix(d).is_some_and(|p| p.ends_with('.')))
            })
    }

    /// Port of `NET_DOWN.test` (`main.js:143`) on plain text.
    pub fn net_down_str(error: &str) -> bool {
        NET_DOWN.is_match(error)
    }
}
pub use plain::{allowed_str, net_down_str};

/// Port of `allowed` (`main-pure.js:35`).
pub fn allowed(url: Value) -> Result<Value, String> {
    Ok(allowed_str(&js_string(&url)).into())
}

/// Port of `netDown` (`main-pure.js:36`). An object without a usable `toString` throws in JS (`TypeError`).
pub fn net_down(error: Value) -> Result<Value, String> {
    if error
        .as_object()
        .is_some_and(|o| o.contains_key("toString") && !o["toString"].is_object())
    {
        return Err("TypeError: Cannot convert object to primitive value".into());
    }
    Ok(net_down_str(&js_string(&error)).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_and_lookalikes() {
        assert!(allowed_str("https://tinder.com/app/recs"));
        assert!(allowed_str("https://WWW.Tinder.COM:8443/x"));
        assert!(allowed_str("  https://a.b.gotinder.com\t/\n"));
        assert!(!allowed_str("http://tinder.com"));
        assert!(!allowed_str("https://evil-tinder.com"));
        assert!(!allowed_str("https://tinder.com.evil.io"));
        // WHATWG: `\` ends the host of a special scheme, so this host is tinder.com.
        assert!(allowed_str(r"https://tinder.com\@evil.com"));
        assert!(!allowed_str(r"https://evil.com\@tinder.com"));
        assert!(allowed_str("https://xn--.tinder.com"));
        assert!(!allowed_str(""));
    }

    #[test]
    fn net_down_alternatives() {
        assert!(net_down_str("net::ERR_TIMED_OUT"));
        assert!(net_down_str("ERR_CONNECTION_RESET"));
        assert!(!net_down_str("net::ERR_ABORTED"));
        assert!(!net_down_str("ERR_TIMED_OUTX"));
        assert!(!net_down_str("ERR_TIMED_OUT\n"));
        assert!(!net_down_str("err_timed_out"));
    }
}
