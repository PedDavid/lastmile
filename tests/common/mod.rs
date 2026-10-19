//! Shared test harness for the HTTP tests.
#![allow(dead_code)]

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum_extra::extract::PrivateCookieJar;
use axum_extra::extract::cookie::{Cookie, Key};
use clap::Parser;
use http_body_util::BodyExt;

use lastmile::config::Config;
use lastmile::db::Db;
use lastmile::web::auth::{AuthMode, SESSION_COOKIE, Session};
use lastmile::web::{AppState, router};

pub const ORIGIN: &str = "http://localhost:8080";

/// Every relative date ("3h ago", "Tomorrow") in the demo app is computed
/// against this.
pub const NOW: &str = "2026-10-18T12:00:00Z";

pub struct Harness {
    pub app: Router,
    pub db: Db,
    pub key: Key,
}

/// The app behind OIDC (never contacted: tests bring their own session
/// cookie), on an empty database.
pub fn harness() -> Harness {
    let cfg = Config::try_parse_from([
        "lastmile",
        "--oidc-issuer",
        "https://idp.example.com",
        "--oidc-client-id",
        "lastmile",
    ])
    .unwrap();
    let db = Db::open_in_memory().unwrap();
    let auth = AuthMode::from_config(&cfg.auth, &cfg.public_url, false).unwrap();
    let key = Key::generate();
    let state = AppState::new(cfg, db.clone(), auth, key.clone());
    Harness {
        app: router(state.with_fixed_now(NOW.parse().unwrap())),
        db,
        key,
    }
}

/// [`harness`] with the rule-drafting model at `llm_url` (an OpenAI-style
/// API, normally a mock).
pub fn harness_llm(llm_url: &str) -> Harness {
    harness_analyzer(llm_url, None)
}

/// [`harness_llm`] with a Presidio analyzer at `analyzer_url` (normally a
/// mock).
pub fn harness_analyzer(llm_url: &str, analyzer_url: Option<&str>) -> Harness {
    let mut args = vec![
        "lastmile",
        "--oidc-issuer",
        "https://idp.example.com",
        "--oidc-client-id",
        "lastmile",
        "--llm-model",
        "test-model",
        "--llm-base-url",
        llm_url,
    ];
    if let Some(url) = analyzer_url {
        args.extend(["--presidio-url", url]);
    }
    let cfg = Config::try_parse_from(args).unwrap();
    let db = Db::open_in_memory().unwrap();
    let auth = AuthMode::from_config(&cfg.auth, &cfg.public_url, false).unwrap();
    let key = Key::generate();
    let llm = lastmile::llm::Client::from_config(&cfg.llm).unwrap();
    let analyzer = lastmile::analyzer::Client::from_config(&cfg.analyzer).unwrap();
    let state = AppState::new(cfg, db.clone(), auth, key.clone())
        .with_llm(llm)
        .with_analyzer(analyzer);
    Harness {
        app: router(state.with_fixed_now(NOW.parse().unwrap())),
        db,
        key,
    }
}

pub fn session(admin: bool) -> Session {
    let now = jiff::Timestamp::now().as_second();
    Session {
        sub: if admin { "u1" } else { "u2" }.into(),
        name: if admin { "ana" } else { "rui" }.into(),
        admin,
        csrf: "csrf-token".into(),
        exp: now + 3600,
    }
}

/// Encrypts a session exactly as the login callback does.
pub fn session_cookie(key: &Key, session: &Session) -> String {
    let jar = PrivateCookieJar::new(key.clone()).add(Cookie::new(
        SESSION_COOKIE,
        serde_json::to_string(session).unwrap(),
    ));
    let res = (jar, ()).into_response();
    let set_cookie = res.headers()[header::SET_COOKIE].to_str().unwrap();
    set_cookie.split(';').next().unwrap().to_string()
}

pub async fn body(res: axum::response::Response) -> String {
    String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap()
}

/// The `--demo` app (sample packages, dev auth) frozen at [`NOW`], with a
/// fixed CSRF token so its pages render byte for byte the same every run.
pub fn demo_app() -> Router {
    demo_app_at("http://localhost:8080")
}

/// [`demo_app`] served at `public_url` (the Origin its forms post from).
pub fn demo_app_at(public_url: &str) -> Router {
    demo_app_llm(public_url, "http://llm.invalid/v1")
}

/// [`demo_app_at`] with the rule-drafting model at `llm_url`.
pub fn demo_app_llm(public_url: &str, llm_url: &str) -> Router {
    let now: jiff::Timestamp = NOW.parse().unwrap();
    let cfg = Config::try_parse_from([
        "lastmile",
        "--demo",
        "--public-url",
        public_url,
        "--llm-model",
        "demo-model",
        "--llm-base-url",
        llm_url,
    ])
    .unwrap();
    let db = Db::open_in_memory().unwrap();
    db.blocking(|c| lastmile::demo::load(c, now)).unwrap();
    let auth = AuthMode::InsecureDev {
        csrf: "demo-csrf-token".into(),
    };
    // A model is "configured" so the draft buttons show; nothing in the
    // screenshots ever calls it.
    let llm = lastmile::llm::Client::from_config(&cfg.llm).unwrap();
    let state = AppState::new(cfg, db, auth, Key::generate()).with_llm(llm);
    router(state.with_fixed_now(now))
}
