//! test/engine-driver.js ported: the engine LOGIC against a pure FAKE driver, no Chromium, no Tinder.
//! Each original assertion message is kept as a comment next to its assert.
mod common;

use std::{cell::RefCell, rc::Rc};

use common::{FakeOpts, FakeTinder, env, fast};
use serde_json::Value;
use spark::{
    core::{
        driver_web::WebDriver,
        engine::{Driver, Reason, RoundOpts, Screen, round, state},
    },
    iface::{Fut, Page},
};

fn fake(o: FakeOpts) -> FakeTinder {
    FakeTinder::new(o)
}

fn limit(n: i64) -> FakeOpts {
    FakeOpts {
        limit: Some(n),
        ..Default::default()
    }
}

type Deltas = Rc<RefCell<Vec<i64>>>;

fn counter() -> (Deltas, Box<dyn Fn(i64)>) {
    let v = Rc::new(RefCell::new(Vec::new()));
    let w = v.clone();
    (v, Box::new(move |n| w.borrow_mut().push(n)))
}

#[tokio::test]
async fn likes_limit() {
    // ---- limite de likes: 1ª carta descartada, o resto curtido, termina em "out" ----
    let d = fake(limit(4));
    let (likes, on_like) = counter();
    let r = round(
        &d,
        &RoundOpts {
            skip_first: true,
            on_like,
            ..fast()
        },
        &env(1),
    )
    .await
    .unwrap();
    assert_eq!(
        (r.reason, r.likes, likes.borrow().iter().sum::<i64>()),
        (Reason::Out, 4, 4)
    );
    // 'a 1ª carta é descartada (skipFirst); o 5º toque é o que revela o limite'
    assert_eq!(
        d.t.borrow().swipes,
        ["nope", "like", "like", "like", "like", "like"]
    );
    // 'o Tinder aceitou 4; o motor descontou o toque que não pegou (r.likes === 4)'
    assert_eq!(d.t.borrow().likes, 4);
    {
        // a contagem nunca sobe e volta: só entram likes confirmados, sempre em deltas positivos
        let d2 = fake(limit(4));
        let (deltas, on_like) = counter();
        let r2 = round(
            &d2,
            &RoundOpts {
                skip_first: true,
                on_like,
                ..fast()
            },
            &env(2),
        )
        .await
        .unwrap();
        // `sem delta negativo: ${deltas}`
        assert!(
            deltas.borrow().iter().all(|n| *n > 0),
            "sem delta negativo: {:?}",
            deltas.borrow()
        );
        assert_eq!(deltas.borrow().iter().sum::<i64>(), d2.t.borrow().likes);
        assert_eq!(r2.likes, d2.t.borrow().likes);
    }
}

#[tokio::test]
async fn without_skipping_the_first_card() {
    let d = fake(limit(3));
    let r = round(&d, &fast(), &env(3)).await.unwrap();
    // ok('sem pular a 1ª carta')
    assert_eq!(
        (r.reason, r.likes, d.t.borrow().swipes[0]),
        (Reason::Out, 3, "like")
    );
}

#[tokio::test]
async fn unlimited_likes_offer_is_the_limit() {
    // ---- oferta de curtidas ilimitadas = limite; o motor não "compra" nem fecha a oferta ----
    let d = fake(FakeOpts {
        limit: Some(2),
        offer: true,
        ..Default::default()
    });
    let r = round(&d, &fast(), &env(4)).await.unwrap();
    let t = d.t.borrow();
    // ok('oferta de curtidas ilimitadas: limite detectado, nada clicado')
    assert_eq!(
        (r.reason, r.likes, t.dismissed, t.screen),
        (Reason::Out, 2, 0, "offer")
    );
}

#[tokio::test]
async fn match_once_dismiss_go_on() {
    // ---- match: avisa uma vez, fecha o popup, segue ----
    let d = fake(FakeOpts {
        limit: Some(5),
        match_at: Some(2),
        ..Default::default()
    });
    let matches = Rc::new(std::cell::Cell::new(0));
    let m = matches.clone();
    let r = round(
        &d,
        &RoundOpts {
            on_match: Box::new(move || m.set(m.get() + 1)),
            ..fast()
        },
        &env(5),
    )
    .await
    .unwrap();
    // ok('match: 1 aviso, 1 fechamento, rodada continua até o limite')
    assert_eq!(
        (r.reason, r.likes, matches.get(), d.t.borrow().dismissed),
        (Reason::Out, 5, 1, 1)
    );
}

#[tokio::test]
async fn stubborn_popup_gives_up() {
    // ---- popup que não fecha: desiste (stuck) em vez de ficar em laço ----
    let d = fake(limit(9));
    d.t.borrow_mut().screen = "popup";
    d.stubborn.set(true);
    let r = round(
        &d,
        &RoundOpts {
            skip_first: true,
            ..fast()
        },
        &env(6),
    )
    .await
    .unwrap();
    // JSON.stringify(r); ok('popup teimoso: 8 tentativas de fechar e "stuck", sem laço infinito')
    assert_eq!(
        (r.reason, r.likes, d.t.borrow().dismissed),
        (Reason::Stuck, 0, 8),
        "{r:?}"
    );
}

#[tokio::test]
async fn login_empty_stuck_stop_gone() {
    // ---- login / vazio / travado / pausa / sessão acabou ----
    {
        let d = fake(FakeOpts {
            login: true,
            ..Default::default()
        });
        let r = round(&d, &fast(), &env(7)).await.unwrap();
        assert_eq!((r.reason, d.t.borrow().swipes.len()), (Reason::Login, 0));
        assert!(!d.logged_in().await.unwrap());
        d.t.borrow_mut().screen = "card";
        // ok('login: a rodada para sem tocar em nada; loggedIn() acompanha')
        assert!(d.logged_in().await.unwrap());
    }
    {
        let d = fake(FakeOpts {
            empty_after: Some(1),
            ..Default::default()
        });
        let r = round(
            &d,
            &RoundOpts {
                skip_first: true,
                per_load: Some(30),
                ..fast()
            },
            &env(8),
        )
        .await
        .unwrap();
        // ok('sem mais pessoas: "empty"')
        assert_eq!((r.reason, d.t.borrow().swipes.len()), (Reason::Empty, 1));
    }
    {
        let d = fake(FakeOpts {
            freeze: true,
            ..Default::default()
        });
        let r = round(
            &d,
            &RoundOpts {
                skip_first: true,
                ..fast()
            },
            &env(9),
        )
        .await
        .unwrap();
        assert_eq!(r.reason, Reason::Stuck);
        // 'curtidas que não pegaram (tela não mudou) são descontadas'
        assert_eq!(r.likes, 0);
    }
    {
        let d = fake(FakeOpts::default());
        let n = Rc::new(std::cell::Cell::new(0));
        let r = round(
            &d,
            &RoundOpts {
                skip_first: true,
                stop: Box::new(move || {
                    n.set(n.get() + 1);
                    n.get() > 3
                }),
                ..fast()
            },
            &env(10),
        )
        .await
        .unwrap();
        assert_eq!(r.reason, Reason::Stopped);
        let g = fake(FakeOpts::default());
        g.t.borrow_mut().gone = true;
        assert_eq!(
            round(&g, &fast(), &env(11)).await.unwrap().reason,
            Reason::Stopped
        );
        // ok('pausa e sessão encerrada (gone): "stopped", sem swipe')
        assert_eq!(g.t.borrow().swipes.len(), 0);
    }
}

#[tokio::test]
async fn radar_waits_for_the_card() {
    // ---- carta carregando (radar): não swipa no escuro ----
    let d = fake(FakeOpts {
        limit: Some(2),
        loading: 4,
        ..Default::default()
    });
    let r = round(
        &d,
        &RoundOpts {
            skip_first: true,
            ..fast()
        },
        &env(12),
    )
    .await
    .unwrap();
    assert_eq!((r.reason, r.likes), (Reason::Out, 2));
    // ok('radar: espera a carta carregar antes de qualquer swipe')
    assert!(d.t.borrow().observed > 4);
}

#[tokio::test]
async fn per_load_reload_after_each_like() {
    // ---- perLoad: uma carta descartada, uma curtida, depois pede recarga (reload é do chamador) ----
    let d = fake(limit(4));
    let (mut loads, mut total) = (Vec::new(), 0);
    let r = loop {
        d.reload().await;
        let r = round(
            &d,
            &RoundOpts {
                skip_first: true,
                per_load: Some(2),
                ..fast()
            },
            &env(13),
        )
        .await
        .unwrap();
        total += r.likes;
        let swipes = std::mem::take(&mut d.t.borrow_mut().swipes);
        loads.push(
            swipes
                .iter()
                .map(|k| if *k == "nope" { "L" } else { "R" })
                .collect::<String>(),
        );
        if r.reason != Reason::Reload {
            break r;
        }
    };
    assert_eq!((r.reason, total), (Reason::Out, 4));
    // ok(`recarga a cada like: ${loads.join(' | ')}`)
    assert!(loads.iter().all(|l| l.len() <= 2), "{loads:?}");
}

#[tokio::test]
async fn state_classifies() {
    // ---- state(): classificação a partir do que o driver vê ----
    let d = fake(FakeOpts::default());
    for (screen, s) in [
        ("out", Screen::Out),
        ("offer", Screen::Out),
        ("match", Screen::Match),
        ("popup", Screen::Popup),
        ("empty", Screen::Empty),
        ("card", Screen::Ok),
    ] {
        d.t.borrow_mut().screen = screen;
        // `${screen} -> ${s}`
        assert_eq!(state(&d).await.unwrap().s, s, "{screen} -> {s:?}");
    }
    d.t.borrow_mut().screen = "login";
    // ok('state: out/offer/match/popup/empty/ok/login')
    assert!(state(&d).await.unwrap().login);
}

/// `wc = { executeJavaScript: async () => ({}), isDestroyed: () => false }`
struct RawPage;
impl Page for RawPage {
    fn eval(&self, _script: &str) -> Fut<'_, Result<Value, String>> {
        Box::pin(async { Ok(Value::Object(Default::default())) })
    }
    fn key(&self, _key_code: &'static str) -> Fut<'_, ()> {
        Box::pin(async {})
    }
    fn load(&self, _url: &str) -> Fut<'_, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    fn url(&self) -> String {
        String::new()
    }
    fn destroyed(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn driver_contract() {
    // ---- contrato: o que um driver precisa ter ----
    // REQUIRED = observe/settle/swipe/dismiss/gone/reload/loggedIn: `driver ${name} sem ${m}()` is a compile-time
    // guarantee here (both implement engine::Driver); `driver ${name}: gone() devolve booleano` likewise.
    let web = WebDriver::new(Rc::new(RawPage), "https://tinder.com/app/recs");
    let drivers: [(&str, &dyn Driver); 2] = [("falso", &fake(FakeOpts::default())), ("web", &web)];
    for (name, drv) in drivers {
        let gone: bool = drv.gone();
        assert!(!gone, "driver {name}: gone() devolve booleano");
    }
    // 'webContents cru vira driver web': WebDriver::new over an iface::Page; 'driver pronto passa direto': any
    // engine::Driver is used as is (no asDriver needed in Rust)
    assert!(web.observe().await.is_ok());
    // ok('contrato: falso e web têm observe/settle/swipe/dismiss/gone/reload/loggedIn; webContents cru vira driver web')
}
