//! Tests for the JS regex semantics (traps) of `src/port.rs`.

use spark_patterns::port;

// `&str` in, `Result<bool, _>` out, through the bridged `Value` API.
macro_rules! wrap {
        ($($n:ident),*) => {$(
            fn $n(t: &str) -> Result<bool, String> {
                port::$n(t.into()).map(|v| v == true)
            }
        )*};
    }
wrap!(
    out_re,
    likes_offer_re,
    plan_re,
    empty_re,
    match_re,
    dismiss_re
);

#[test]
fn plain_and_case() {
    assert_eq!(out_re("You're out of Likes"), Ok(true));
    assert_eq!(out_re("Você ficou sem Likes! Volte em 11:59:00"), Ok(true));
    assert_eq!(out_re("SEM CURTIDAS"), Ok(true));
    assert_eq!(
        likes_offer_re("Curtidas ilimitadas. Curta quantas vezes quiser."),
        Ok(true)
    );
    assert_eq!(plan_re("Escolha um plano"), Ok(true));
    assert_eq!(plan_re("LIKE AS MUCH AS YOU WANT"), Ok(true));
    assert_eq!(empty_re("There's no one new around you"), Ok(true));
    assert_eq!(empty_re("NINGUÉM NOVO"), Ok(true));
    assert_eq!(empty_re("NÃO HÁ MAIS NINGUÉM"), Ok(true));
    assert_eq!(empty_re("ninguem novo"), Ok(true));
    assert_eq!(match_re("It's a Match!"), Ok(true));
    assert_eq!(match_re("ITS A MATCH"), Ok(true));
    assert_eq!(match_re("É UM MATCH"), Ok(true));
    assert_eq!(
        match_re("Se você curte áudio longo e rir de bobeira, já deu match."),
        Ok(true)
    );
    assert_eq!(out_re("sem likes de novo"), Ok(true));
    assert_eq!(out_re("Tinder"), Ok(false));
}

#[test]
fn dismiss_anchored() {
    for t in [
        "Not interested",
        "Agora não",
        "NÃO TENHO INTERESSE",
        "Não, obrigada",
        "Back to Tinder",
    ] {
        assert_eq!(dismiss_re(t), Ok(true), "{t}");
    }
    assert_eq!(dismiss_re("  Got it  "), Ok(false));
    assert_eq!(dismiss_re("Got it"), Ok(true));
    assert_eq!(
        dismiss_re("Got it\n"),
        Ok(false),
        "JS $ does not match before a final newline"
    );
    assert_eq!(dismiss_re("\nGot it"), Ok(false));
    assert_eq!(dismiss_re("got it!"), Ok(false));
    assert_eq!(dismiss_re("a\nnot now"), Ok(false));
}

#[test]
fn non_ascii_letters_never_fold_to_ascii() {
    assert_eq!(out_re("sem l\u{131}kes"), Ok(false));
    assert_eq!(out_re("sem l\u{130}kes"), Ok(false));
    assert_eq!(dismiss_re("\u{212A}eep swiping"), Ok(false));
    assert_eq!(dismiss_re("keep \u{17F}wiping"), Ok(false));
    assert_eq!(plan_re("\u{17F}colha um plano"), Ok(false));
    assert_eq!(likes_offer_re("unlimited \u{212A}ikes"), Ok(false));
}

#[test]
fn js_whitespace_set() {
    for w in [
        "\u{A0}", "\u{FEFF}", "\u{2028}", "\u{2029}", "\u{3000}", "\u{1680}", "\u{2000}",
        "\u{200A}", "\u{202F}", "\u{205F}", "\t", "\n", "\r", "\u{B}", "\u{C}",
    ] {
        assert_eq!(plan_re(&format!("escolha{w}um{w}plano")), Ok(true), "{w:?}");
    }
    for w in ["\u{85}", "\u{200B}", "\u{180E}", ""] {
        assert_eq!(
            plan_re(&format!("escolha{w}um{w}plano")),
            Ok(false),
            "{w:?}"
        );
    }
    // Literal spaces stay spaces in patterns that do not use `\s`.
    assert_eq!(out_re("out\u{A0}of likes"), Ok(false));
}

#[test]
fn non_string_arguments_are_stringified_like_js() {
    use serde_json::json;
    let t = |v: serde_json::Value| port::dismiss_re(v).unwrap() == true;
    assert!(t(json!(["got it"])));
    assert!(!t(json!([null, "x"])));
    assert!(!t(json!(0)));
    assert!(!t(json!(null)));
    assert!(!t(json!({"a": "got it"})));
    assert!(!t(json!(true)));
    assert!(!t(json!([["got it"], 1])));
}

#[test]
fn accents_and_decomposition() {
    assert_eq!(empty_re("ningue\u{301}m novo"), Ok(false));
    assert_eq!(empty_re("ningu\u{C8}m novo"), Ok(false));
    assert_eq!(empty_re("ningu\u{C9}m novo"), Ok(true));
    assert_eq!(empty_re("n\u{C3}o h\u{C1} mais ningu\u{C9}m"), Ok(true));
    assert_eq!(match_re("e\u{301} um match"), Ok(false));
    assert_eq!(match_re("itss a match"), Ok(false));
    assert_eq!(match_re("it\u{2019}s a match"), Ok(false));
}
