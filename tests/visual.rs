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
use ui::{open, open_llm, open_phone};

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

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn inbox() {
    let ui = open("/inbox", &[]).await;
    assert_screenshot(&ui.page, "inbox").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn inbox_phone_dark() {
    let ui = open_phone("/inbox", &[("lastmile_theme", "dark")]).await;
    assert_screenshot(&ui.page, "inbox-phone-dark").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn rules_list() {
    let ui = open("/rules", &[]).await;
    assert_screenshot(&ui.page, "rules").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn rule_editor_from_an_inbox_message() {
    // Message 3: a pickup notice the CTT rule matches but can't read a
    // tracking number from.
    let ui = open("/rules/new?message=3", &[]).await;
    expect(ui.page.locator("#preview .card").first())
        .to_be_visible()
        .await
        .unwrap();
    assert_screenshot(&ui.page, "rule-editor").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn share_target_phone() {
    let ui = open_phone(
        "/share?title=CTT&text=CTT%3A%20A%20sua%20encomenda%20RR123456789PT%20est%C3%A1%20dispon%C3%ADvel%20para%20levantamento%20no%20Cacifo%20CTT%20Mercado%20de%20Benfica.%20C%C3%B3digo%3A%20482913.",
        &[],
    )
    .await;
    assert_screenshot(&ui.page, "share-phone").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn settings_devices() {
    let ui = open("/settings", &[]).await;
    ui.page
        .locator("input[name=name]")
        .fill("Ana's iPhone", None)
        .await
        .unwrap();
    ui.page
        .locator("form[action='/settings/tokens'] button[type=submit]")
        .click(None)
        .await
        .unwrap();
    expect(ui.page.locator("[data-copy^=lm_]"))
        .to_be_visible()
        .await
        .unwrap();
    // The token is random: cover it so the screenshot is stable.
    ui.page
        .evaluate_expression(
            "document.querySelectorAll('code').forEach(c => { if (c.textContent.startsWith('lm_')) c.textContent = 'lm_' + 'x'.repeat(43); })",
        )
        .await
        .unwrap();
    assert_screenshot(&ui.page, "settings-devices").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn draft_page_shows_what_would_be_sent() {
    // Message 6 names the household, its address, phone and email.
    let ui = open("/rules/draft?message=6", &[]).await;
    assert_screenshot(&ui.page, "draft-rule").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn drafted_rule_in_the_editor() {
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let draft = json!({
        "explanation": "Paack's text the day before delivery: it names the order and the four digit code to give the courier.",
        "slug": "paack-delivery-tomorrow",
        "name": "Paack – delivery tomorrow (SMS)",
        "courier": "Paack",
        "channels": ["share", "paste"],
        "sender": [],
        "all": ["(?i)paack", "(?i)entregue amanh[ãa]"],
        "none": [],
        "extract": [
            {"field": "tracking_number", "pattern": "(?i)encomenda (?P<v>PCK[A-Z0-9]+)"},
            {"field": "merchant", "pattern": "(?i)encomenda PCK[A-Z0-9]+ da (?P<v>\\p{Lu}[\\p{L}]+)"},
            {"field": "code", "pattern": "(?i)c[oó]digo de entrega (?P<v>\\d{4})"},
        ],
        "status": "in_transit",
        "code_kind": "delivery",
        "priority": 100,
        "follow_up": false,
    });
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": draft.to_string()}}],
            "usage": {"prompt_tokens": 812, "completion_tokens": 190},
        })))
        .mount(&server)
        .await;
    let ui = open_llm("/rules/draft?message=6", &format!("{}/v1", server.uri())).await;
    ui.page
        .locator("input[name=hint]")
        .fill(
            "Paack, delivery tomorrow: the 4 digits are the code for the courier",
            None,
        )
        .await
        .unwrap();
    ui.page
        .locator("button[value=send]")
        .click(None)
        .await
        .unwrap();
    expect(ui.page.locator("#preview .card").first())
        .to_be_visible()
        .await
        .unwrap();
    assert_screenshot(&ui.page, "draft-editor").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn export_a_rule() {
    let id = lastmile::demo::rule_id("paack-out-for-delivery");
    let ui = open(&format!("/rules/{id}/export"), &[]).await;
    assert_screenshot(&ui.page, "rule-export").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn import_preview_of_an_update() {
    use lastmile::rules::share;
    // A newer copy of the demo's Paack rule: it also excludes "tomorrow".
    let mut doc = share::parse(include_str!(
        "fixtures/shared/paack-out-for-delivery.lastmile.yaml"
    ))
    .unwrap();
    doc.id = lastmile::demo::rule_id("paack-out-for-delivery");
    doc.version = 4;
    doc.def.priority = 120;
    doc.def.matcher.none = vec!["(?i)amanh[ãa]".into()];
    let ui = open("/rules/import", &[]).await;
    ui.page
        .locator("textarea[name=text]")
        .fill(&doc.to_yaml(), None)
        .await
        .unwrap();
    ui.page
        .locator("form[action='/rules/import'] button[type=submit]")
        .click(None)
        .await
        .unwrap();
    expect(ui.page.locator(".diff"))
        .to_be_visible()
        .await
        .unwrap();
    assert_screenshot(&ui.page, "rule-import").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn settings_household() {
    let ui = open("/settings", &[]).await;
    ui.page
        .evaluate_expression("document.getElementById('names').scrollIntoView({block: 'start'})")
        .await
        .unwrap();
    assert_screenshot(&ui.page, "settings-household").await;
}

#[tokio::test]
#[ignore = "needs a Playwright browser: make visual"]
async fn redaction_chips_can_be_revealed_and_selected_text_hidden() {
    let ui = open("/rules/draft?message=6", &[]).await;
    expect(ui.page.locator("[data-kind=CODE]").first())
        .to_be_visible()
        .await
        .unwrap();
    ui.page
        .locator("[data-kind=CODE]")
        .first()
        .click(None)
        .await
        .unwrap();
    expect(ui.page.locator("[data-reveal-panel]"))
        .to_be_visible()
        .await
        .unwrap();
    ui.page
        .locator("[data-reveal-confirm]")
        .click(None)
        .await
        .unwrap();
    ui.page.evaluate_expression(r#"(() => {
        const editor = document.querySelector('[data-redaction-editor]');
        const input = document.querySelector('textarea[name=redacted]');
        if (!input.value.includes('5527')) throw Error('Code was not revealed');
        editor.focus();
        const node = Array.from(editor.childNodes).find(n => n.nodeType === 3 && n.textContent.includes('Paack'));
        const start = node.textContent.indexOf('Paack');
        const range = document.createRange();
        range.setStart(node, start); range.setEnd(node, start + 5);
        const selection = window.getSelection();
        selection.removeAllRanges(); selection.addRange(range);
    })()"#).await.unwrap();
    ui.page
        .locator("[data-hide-selection]")
        .click(None)
        .await
        .unwrap();
    expect(ui.page.locator("[data-kind=CUSTOM]"))
        .to_be_visible()
        .await
        .unwrap();
    ui.page.evaluate_expression(r#"(() => {
        const value = document.querySelector('textarea[name=redacted]').value;
        if (!value.includes('‹CUSTOM_1›') || value.includes('Paack')) throw Error('Selection was not hidden');
    })()"#).await.unwrap();
    ui.page
        .locator("button[value=original]")
        .click(None)
        .await
        .unwrap();
    expect(ui.page.locator("button[value=redact]"))
        .to_be_visible()
        .await
        .unwrap();
    ui.page
        .locator("button[value=redact]")
        .click(None)
        .await
        .unwrap();
    expect(ui.page.locator("[data-kind=CODE]").first())
        .to_be_visible()
        .await
        .unwrap();
}
