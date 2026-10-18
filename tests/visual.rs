//! Pixel screenshots of the `--demo` UI in Chromium (see `ui/mod.rs`),
//! compared with the PNGs in `tests/screenshots/`.
//!
//! They need Playwright's Chromium, so plain `cargo test` skips them.
//! Fonts and antialiasing differ between machines, so they run in a pinned
//! container (CI's `visual` job): `make visual-docker` checks them and
//! `make visual-docker-update` rewrites the baselines from any machine with
//! Docker. On failure the capture is kept as `{name}-actual.png`, and CI
//! uploads the lot as the `screenshots` artifact.

mod common;
mod ui;

use std::path::PathBuf;

use playwright_rs::protocol::Page;
use playwright_rs::{Animations, ScreenshotAssertionOptions, expect, expect_page};
use ui::{open, open_phone};

/// Compares the viewport with `tests/screenshots/{name}.png`. A missing
/// baseline fails rather than being written, unless `UPDATE_SNAPSHOTS` is set;
/// the capture is kept next to it as `{name}-actual.png` either way.
async fn assert_screenshot(page: &Page, name: &str) {
    let dir: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "screenshots"]
        .iter()
        .collect();
    let baseline = dir.join(format!("{name}.png"));
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();
    let options = |update| {
        ScreenshotAssertionOptions::builder()
            .animations(Animations::Disabled)
            .update_snapshots(update)
            .build()
    };
    if !update && !baseline.exists() {
        let actual = dir.join(format!("{name}-actual.png"));
        let _ = expect_page(page)
            .to_have_screenshot(&actual, Some(options(true)))
            .await;
        panic!(
            "no baseline at {}; the capture is in {}",
            baseline.display(),
            actual.display()
        );
    }
    if let Err(e) = expect_page(page)
        .to_have_screenshot(&baseline, Some(options(update)))
        .await
    {
        panic!("{name}: {e}");
    }
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn packages() {
    let ui = open("/", &[]).await;
    assert_screenshot(&ui.page, "packages").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn packages_phone() {
    let ui = open_phone("/", &[]).await;
    assert_screenshot(&ui.page, "packages-phone").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn packages_dark_violet() {
    let ui = open(
        "/",
        &[("lastmile_theme", "dark"), ("lastmile_palette", "violet")],
    )
    .await;
    assert_screenshot(&ui.page, "packages-dark-violet").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn command_palette() {
    let ui = open("/", &[]).await;
    ui.page.keyboard().press("Control+k", None).await.unwrap();
    expect(ui.page.locator("#command-results [role=option]").first())
        .to_be_visible()
        .await
        .unwrap();
    assert_screenshot(&ui.page, "command-palette").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn filtered_needs_attention() {
    let ui = open("/", &[]).await;
    ui.page
        .locator(".filter-chip[data-filter=attention]")
        .click(None)
        .await
        .unwrap();
    // Wait for htmx to swap in the filtered list and the updated chips.
    expect(
        ui.page
            .locator(".filter-chip[data-filter=attention][data-active=true]"),
    )
    .to_be_visible()
    .await
    .unwrap();
    assert_screenshot(&ui.page, "filtered-attention").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn detail_ready_for_pickup() {
    let ui = open("/packages/1", &[]).await;
    assert_screenshot(&ui.page, "detail-pickup").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn detail_phone_dark() {
    let ui = open_phone("/packages/2", &[("lastmile_theme", "dark")]).await;
    assert_screenshot(&ui.page, "detail-phone-dark").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn status_dialog() {
    let ui = open("/packages/5", &[]).await;
    ui.page
        .locator("[data-dialog-open=status-dialog]")
        .click(None)
        .await
        .unwrap();
    expect(ui.page.locator("#status-title"))
        .to_be_visible()
        .await
        .unwrap();
    assert_screenshot(&ui.page, "status-dialog").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn add_package_phone() {
    let ui = open_phone("/packages/new", &[]).await;
    assert_screenshot(&ui.page, "add-package-phone").await;
}
