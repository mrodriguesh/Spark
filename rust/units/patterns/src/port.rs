//! The Rust port of `patterns.js`: JS `/i` regexes WITHOUT the `u` flag. ASCII letters match only their ASCII
//! case pair, `é á ã` also match `É Á Ã`, no other non-ASCII char folds to a letter (Kelvin sign, long s, dotless
//! i), `\s` is the JS set (no U+0085), `^`/`$` only touch the ends of the text. So the Rust regexes are
//! case-SENSITIVE and `build` spells every case pair out.

#![forbid(unsafe_code)]

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

/// JS `\s` (ECMAScript WhiteSpace + LineTerminator).
const JS_SPACE: &str = r"[\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]";

/// JS source of a `/i` pattern (only `\s` is escaped) -> case-sensitive Rust pattern.
fn build(src: &str) -> Regex {
    let (mut out, mut set, mut chars) = (String::new(), false, src.chars());
    while let Some(c) = chars.next() {
        let up = c.to_uppercase().next().unwrap_or(c);
        if c == '\\' {
            chars.next();
            out.push_str(JS_SPACE);
        } else if up == c || c == '[' || c == ']' {
            set = if c == '[' || c == ']' { c == '[' } else { set };
            out.push(c);
        } else if set {
            out.extend([c, up]);
        } else {
            out.extend(['[', c, up, ']']);
        }
    }
    // INFALLIBLE: constant patterns of this file, exercised by every test.
    Regex::new(&out).expect("valid pattern")
}

/// JS `String(value)` for a JSON value: what `RegExp.prototype.test` does to a non-string argument.
fn js_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|e| {
                if e.is_null() {
                    String::new()
                } else {
                    js_string(e)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
        other => other.to_string(),
    }
}

static OUT: LazyLock<Regex> = LazyLock::new(|| {
    build(
        r"out of likes|sem (likes|curtidas)|acabaram (os|as|seus|suas) (likes|curtidas)|ficou sem (likes|curtidas)|likes acabaram|curtidas acabaram",
    )
});
static LIKES_OFFER: LazyLock<Regex> =
    LazyLock::new(|| build(r"(?:curtidas|likes)\s+ilimitad[oa]s|unlimited\s+likes"));
static PLAN: LazyLock<Regex> = LazyLock::new(|| {
    build(
        r"escolha\s+um\s+plano|curta\s+quantas\s+vezes\s+quiser|choose\s+a\s+plan|like\s+as\s+much\s+as\s+you\s+want",
    )
});
static EMPTY: LazyLock<Regex> = LazyLock::new(|| {
    build(
        r"no one new|run out of potential|ningu[eé]m novo|n[aã]o h[aá] mais ningu[eé]m|acabaram as pessoas",
    )
});
static MATCH: LazyLock<Regex> = LazyLock::new(|| build(r"it'?s a match|deu match|[eé] um match"));
static DISMISS: LazyLock<Regex> = LazyLock::new(|| {
    build(
        r"^(not interested|n[aã]o tenho interesse|agora n[aã]o|not now|maybe later|talvez depois|no thanks|n[aã]o, obrigad[oa]|entendi|got it|back to tinder|voltar ao tinder|keep swiping|continuar deslizando)$",
    )
});

/// `outRe` (`patterns.js:5`).
pub fn out_re(text: Value) -> Result<Value, String> {
    Ok(OUT.is_match(&js_string(&text)).into())
}

/// `likesOfferRe` (`patterns.js:6`).
pub fn likes_offer_re(text: Value) -> Result<Value, String> {
    Ok(LIKES_OFFER.is_match(&js_string(&text)).into())
}

/// `planRe` (`patterns.js:7`).
pub fn plan_re(text: Value) -> Result<Value, String> {
    Ok(PLAN.is_match(&js_string(&text)).into())
}

/// `emptyRe` (`patterns.js:8`).
pub fn empty_re(text: Value) -> Result<Value, String> {
    Ok(EMPTY.is_match(&js_string(&text)).into())
}

/// `matchRe` (`patterns.js:9`).
pub fn match_re(text: Value) -> Result<Value, String> {
    Ok(MATCH.is_match(&js_string(&text)).into())
}

/// `dismissRe` (`patterns.js:10`): anchored, the caller trims.
pub fn dismiss_re(text: Value) -> Result<Value, String> {
    Ok(DISMISS.is_match(&js_string(&text)).into())
}
