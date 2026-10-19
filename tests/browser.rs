//! The browser client against a real headless Chromium. It needs one, so it
//! only runs when asked to:
//!
//! ```sh
//! docker run -d --network host chromedp/headless-shell:stable
//! LASTMILE_TEST_BROWSER_URL=http://127.0.0.1:9222 cargo test --test browser -- --ignored
//! ```
//!
//! The page it reads is served here and, like a courier's tracking page, has
//! no content in its HTML: a script draws it a moment after the page loads.

use axum::Router;
use axum::response::Html;
use axum::routing::get;
use clap::Parser;
use lastmile::browser::{BrowserError, Client};
use lastmile::config::Config;

const PAGE: &str = r#"<!doctype html><html><body><div id="app"></div>
<script>
  setTimeout(() => {
    document.getElementById('app').innerHTML =
      '<p>Objeto DW000000001PT</p><ul><li>16-10-2026 09:30 Objeto aceite</li></ul>';
  }, 800);
</script></body></html>"#;

async fn serve() -> String {
    let app = Router::new().route("/t/DW000000001PT", get(|| async { Html(PAGE) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://127.0.0.1:{}/t/DW000000001PT", addr.port())
}

fn client(url: &str, timeout: &str) -> Client {
    let cfg = Config::try_parse_from([
        "lastmile",
        "--browser-url",
        url,
        "--browser-timeout",
        timeout,
    ])
    .unwrap();
    Client::from_config(&cfg.browser).expect("a browser is configured")
}

fn browser_url() -> String {
    std::env::var("LASTMILE_TEST_BROWSER_URL").expect("LASTMILE_TEST_BROWSER_URL")
}

#[tokio::test]
#[ignore = "needs a headless Chromium: set LASTMILE_TEST_BROWSER_URL"]
async fn reads_what_a_page_draws_after_it_loads() {
    let page = serve().await;
    let text = client(&browser_url(), "30s")
        .render(&page, |t| t.contains("Objeto aceite"))
        .await
        .unwrap();
    assert!(text.contains("DW000000001PT"), "{text}");
    assert!(text.contains("16-10-2026 09:30 Objeto aceite"), "{text}");
}

#[tokio::test]
#[ignore = "needs a headless Chromium: set LASTMILE_TEST_BROWSER_URL"]
async fn a_page_that_never_shows_what_is_wanted_times_out() {
    let page = serve().await;
    let err = client(&browser_url(), "4s")
        .render(&page, |t| t.contains("never on the page"))
        .await
        .unwrap_err();
    assert!(matches!(err, BrowserError::Timeout(_)), "{err}");
}

/// Chromium refuses a DevTools request whose Host is a name: the client
/// resolves it. Needs `LASTMILE_TEST_BROWSER_NAME_URL`, the same endpoint by a
/// name that is not `localhost` (an /etc/hosts entry will do).
#[tokio::test]
#[ignore = "needs a headless Chromium and a name for it: set LASTMILE_TEST_BROWSER_NAME_URL"]
async fn reaches_the_browser_by_a_service_name() {
    let url =
        std::env::var("LASTMILE_TEST_BROWSER_NAME_URL").expect("LASTMILE_TEST_BROWSER_NAME_URL");
    let page = serve().await;
    let text = client(&url, "30s")
        .render(&page, |t| t.contains("Objeto aceite"))
        .await
        .unwrap();
    assert!(text.contains("DW000000001PT"), "{text}");
}

#[tokio::test]
async fn an_unreachable_browser_is_an_error() {
    let err = client("http://127.0.0.1:1", "5s")
        .render("http://127.0.0.1:1/", |_| true)
        .await
        .unwrap_err();
    assert!(matches!(err, BrowserError::Connect(_)), "{err}");
}
