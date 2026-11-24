//! driver-web.js: the engine's driver over the embedded Tinder page. The page scripts are the original's,
//! byte for byte into `page_scripts.json`.
use std::{rc::Rc, sync::OnceLock};

use serde_json::Value;

use super::{
    engine::{Driver, sleep_ms},
    js,
};
use crate::iface::{Fut, Page};

/// main.js `TINDER`.
pub const HOME: &str = "https://tinder.com/app/recs";
/// `JS_TIMEOUT`: a page script that does not answer in 20 s fails.
pub const JS_TIMEOUT: f64 = 20_000.0;
pub const NO_ANSWER: &str = "a página do Tinder não respondeu";

/// The page scripts: settle = [before JSON.stringify(last), before ms, after ms].
pub struct Scripts {
    pub probe: String,
    pub settle: [String; 3],
    pub swipe_right: String,
    pub swipe_left: String,
    pub dismiss: String,
    /// main.js GEO_PS (PowerShell, not a page script; generated with them).
    pub geo_ps: String,
}

/// The checked-in `page_scripts.json`.
pub const SCRIPTS_JSON: &str = include_str!("page_scripts.json");

fn parse_scripts(text: &str) -> Option<Scripts> {
    let v: Value = serde_json::from_str(text).ok()?;
    let s = |k: &str| v[k].as_str().map(String::from);
    let settle = v["settle"].as_array()?;
    let part = |i: usize| settle.get(i).and_then(Value::as_str).map(String::from);
    Some(Scripts {
        probe: s("probe")?,
        settle: [part(0)?, part(1)?, part(2)?],
        swipe_right: s("swipeRight")?,
        swipe_left: s("swipeLeft")?,
        dismiss: s("dismiss")?,
        geo_ps: s("geoPs")?,
    })
}

pub fn scripts() -> Result<&'static Scripts, String> {
    static S: OnceLock<Option<Scripts>> = OnceLock::new();
    S.get_or_init(|| parse_scripts(SCRIPTS_JSON))
        .as_ref()
        .ok_or_else(|| "page_scripts.json inválido".to_string())
}

/// `settle(wc, last, ms = 500)`'s script.
pub fn settle_script(last: &Value, ms: Option<f64>) -> Result<String, String> {
    let s = &scripts()?.settle;
    Ok(format!(
        "{}{}{}{}{}",
        s[0],
        spark_store::port::stringify(last),
        s[1],
        js::number_to_string(ms.unwrap_or(500.0)),
        s[2]
    ))
}

/// `run(wc, js)`: the script raced against the 20 s timeout (the page's answer is dropped after it).
pub async fn run(page: &dyn Page, script: &str) -> Result<Value, String> {
    tokio::select! {
        r = page.eval(script) => r,
        _ = sleep_ms(JS_TIMEOUT) => Err(NO_ANSWER.into()),
    }
}

/// `{ ...p, login: /tinder\.com/.test(p.url) && !p.url.includes('/app') }`.
fn with_login(p: Value) -> Result<Value, String> {
    let mut o = match p {
        Value::Null => return Err("Cannot read properties of null (reading 'url')".into()),
        Value::Object(o) => o,
        _ => serde_json::Map::new(), // a primitive has no own `url`
    };
    let url = o.get("url").cloned();
    let login = match &url {
        None => false, // String(undefined) has no "tinder.com"
        Some(Value::String(u)) => u.contains("tinder.com") && !u.contains("/app"),
        Some(v) if js::to_string(v).contains("tinder.com") => match v {
            Value::Array(a) => !a.iter().any(|e| e.as_str() == Some("/app")),
            _ => return Err("p.url.includes is not a function".into()),
        },
        Some(_) => false,
    };
    o.insert("login".into(), Value::Bool(login));
    Ok(Value::Object(o))
}

/// `webDriver(wc, { home })`.
pub struct WebDriver {
    page: Rc<dyn Page>,
    home: String,
}

impl WebDriver {
    pub fn new(page: Rc<dyn Page>, home: &str) -> Self {
        WebDriver {
            page,
            home: home.into(),
        }
    }
}

impl Driver for WebDriver {
    fn observe(&self) -> Fut<'_, Result<Value, String>> {
        Box::pin(async move { with_login(run(&*self.page, &scripts()?.probe).await?) })
    }

    fn settle<'a>(&'a self, last: &'a Value, ms: Option<f64>) -> Fut<'a, Result<Value, String>> {
        Box::pin(async move { run(&*self.page, &settle_script(last, ms)?).await })
    }

    fn swipe(&self, right: bool) -> Fut<'_, Result<(), String>> {
        Box::pin(async move {
            let s = scripts()?;
            let clicked = run(
                &*self.page,
                if right { &s.swipe_right } else { &s.swipe_left },
            )
            .await?;
            if !js::truthy(&clicked) {
                self.page.key(if right { "Right" } else { "Left" }).await;
            }
            Ok(())
        })
    }

    fn dismiss(&self) -> Fut<'_, Result<(), String>> {
        Box::pin(async move {
            run(&*self.page, &scripts()?.dismiss).await?;
            sleep_ms(300.0).await;
            self.page.key("Escape").await;
            Ok(())
        })
    }

    fn gone(&self) -> bool {
        self.page.destroyed()
    }

    fn reload(&self) -> Fut<'_, ()> {
        Box::pin(async move {
            let _ = self.page.load(&self.home).await; // redirects abort the promise; fine
        })
    }

    fn logged_in(&self) -> Fut<'_, Result<bool, String>> {
        Box::pin(async move { Ok(self.page.url().contains("/app")) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scripts_load_and_settle_fills_in() {
        let s = scripts().unwrap();
        assert!(s.probe.starts_with("(() => {") && s.dismiss.contains("continuar deslizando"));
        let script = settle_script(&json!("Pessoa 1\n\"25\"\u{2028}"), None).unwrap();
        assert!(
            script.contains("last = \"Pessoa 1\\n\\\"25\\\"\u{2028}\", end = Date.now() + 500")
        );
        assert!(
            settle_script(&Value::Null, Some(2000.0))
                .unwrap()
                .contains("last = null, end = Date.now() + 2000\n")
        );
    }

    #[test]
    fn login_flag() {
        assert_eq!(
            with_login(json!({"url": "https://tinder.com/"})).unwrap()["login"],
            json!(true)
        );
        assert_eq!(
            with_login(json!({"url": "https://tinder.com/app/recs"})).unwrap()["login"],
            json!(false)
        );
        assert_eq!(
            with_login(json!({"url": "fake://tinder"})).unwrap()["login"],
            json!(false)
        );
        assert_eq!(with_login(json!({})).unwrap()["login"], json!(false));
        assert!(with_login(Value::Null).is_err());
    }
}
