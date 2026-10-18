//! A Chromium page on the `--demo` app, for the browser tests. The browser
//! talks to the router in-process (no socket), with the clock pinned like
//! the HTML snapshots. Each test binary using it declares `mod common;` too.
#![allow(dead_code)]

use axum::http::header;
use axum::routing::get;
use playwright_rs::protocol::{
    AddStyleTagOptions, BrowserContext, BrowserContextOptions, Cookie, Page, Playwright, Viewport,
};

pub const ORIGIN: &str = "https://lastmile.test";

/// Dialogs and the palette animate in with `@starting-style`; a screenshot
/// taken during that would catch them half-faded. Served from the app's own
/// origin because its CSP (`style-src 'self'`) refuses inline styles.
const NO_MOTION: &str = "*, *::before, *::after, ::backdrop { \
    transition: none !important; animation: none !important; }";
const NO_MOTION_PATH: &str = "/__test/no-motion.css";

/// A browser page with the app routed in, holding on to what keeps it alive.
pub struct Ui {
    pub page: Page,
    pub context: BrowserContext,
    _playwright: Playwright,
}

/// A laptop screen, and a phone's (the app is mostly opened at the door).
pub const DESKTOP: Viewport = Viewport {
    width: 1280,
    height: 800,
};
pub const PHONE: Viewport = Viewport {
    width: 390,
    height: 844,
};

pub async fn open(path: &str, cookies: &[(&str, &str)]) -> Ui {
    open_app(path, cookies, DESKTOP, crate::common::demo_app()).await
}

pub async fn open_phone(path: &str, cookies: &[(&str, &str)]) -> Ui {
    open_app(path, cookies, PHONE, crate::common::demo_app()).await
}

/// Like [`open`], at `viewport`, serving `app` (the demo app with test-only
/// layers).
pub async fn open_app(
    path: &str,
    cookies: &[(&str, &str)],
    viewport: Viewport,
    app: axum::Router,
) -> Ui {
    let playwright = Playwright::launch().await.expect("launch Playwright");
    let browser = playwright
        .chromium()
        .launch()
        .await
        .expect("launch Chromium (run the tests with `make visual-docker`)");
    let context = browser
        .new_context_with_options(
            BrowserContextOptions::builder()
                .viewport(viewport)
                .device_scale_factor(1.0)
                .locale("en-US".into())
                .timezone_id("UTC".into())
                .reduced_motion("reduce".into())
                .build(),
        )
        .await
        .unwrap();
    let cookies: Vec<Cookie> = cookies
        .iter()
        .map(|(name, value)| {
            let mut c = Cookie::new(*name, *value);
            c.domain = "lastmile.test".into();
            c.path = "/".into();
            c
        })
        .collect();
    context.add_cookies(&cookies).await.unwrap();
    let app = app.route(
        NO_MOTION_PATH,
        get(|| async { ([(header::CONTENT_TYPE, "text/css")], NO_MOTION) }),
    );
    context
        .route_service(&format!("{ORIGIN}/**"), app)
        .await
        .unwrap();
    let page = context.new_page().await.unwrap();
    page.goto(&format!("{ORIGIN}{path}"), None).await.unwrap();
    page.add_style_tag(
        AddStyleTagOptions::builder()
            .url(format!("{ORIGIN}{NO_MOTION_PATH}"))
            .build(),
    )
    .await
    .unwrap();
    Ui {
        page,
        context,
        _playwright: playwright,
    }
}
