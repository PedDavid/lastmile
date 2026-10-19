//! The model that drafts rules (a mock OpenAI-style server), redaction on
//! the way out, the household settings, and sharing rules.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::*;
use lastmile::db::messages::MessageResult;
use lastmile::db::{messages, rules as rule_db, settings};
use lastmile::ingest::{self, Incoming};
use lastmile::model::Channel;
use lastmile::redact::{Household, find_leak};

const ORIGINAL: &str = "Paack: Olá Ana Ferreira, a sua encomenda PCK7Q2W9D da Worten será entregue hoje em Avenida da Liberdade 10, 1250-096 Lisboa. Código de entrega 5527. Dúvidas: 912 345 678 ou ana@example.com.";

fn form(uri: &str, cookie: &str, body: &[(&str, &str)]) -> Request<Body> {
    let body: String = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(body)
        .finish();
    Request::post(uri)
        .header(header::COOKIE, cookie)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body))
        .unwrap()
}

fn get(uri: &str, cookie: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

fn household() -> Household {
    lastmile::demo::household()
}

fn good_draft() -> Value {
    json!({
        "explanation": "Paack's text on the day of delivery, with the four digit delivery code.",
        "slug": "paack-out-for-delivery",
        "name": "Paack – out for delivery (SMS)",
        "courier": "Paack",
        "channels": ["share", "paste"],
        "sender": [],
        "all": ["(?i)paack", "(?i)entregue hoje"],
        "none": [],
        "extract": [
            {"field": "tracking_number", "pattern": "(?i)encomenda (?P<v>PCK[A-Z0-9]+)"},
            {"field": "code", "pattern": "(?i)c[oó]digo de entrega (?P<v>\\d{4})"},
        ],
        "status": "out_for_delivery",
        "code_kind": "delivery",
        "priority": 100,
        "follow_up": false,
    })
}

async fn mock_model(answer: Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": answer.to_string()}}],
            "usage": {"prompt_tokens": 321, "completion_tokens": 87},
        })))
        .mount(&server)
        .await;
    server
}

/// An app with a household, an unmatched message in the inbox and a model.
async fn setup(server: &MockServer) -> (Harness, String) {
    let h = harness_llm(&format!("{}/v1", server.uri()));
    let admin = session_cookie(&h.key, &session(true));
    h.db.blocking(|c| settings::set_household(c, &household()))
        .unwrap();
    h.db.blocking(|c| {
        ingest::ingest(
            c,
            &[],
            Incoming {
                channel: Channel::Share,
                submitted_by: Some("ana".into()),
                sender: None,
                subject: None,
                sent_at: None,
                message_id: None,
                text: ORIGINAL.into(),
                raw: ORIGINAL.as_bytes().to_vec(),
            },
            jiff::SignedDuration::from_hours(24),
            "2026-10-18T12:00:00Z".parse().unwrap(),
        )
    })
    .unwrap();
    (h, admin)
}

#[tokio::test]
async fn the_draft_page_shows_what_would_be_sent_and_sends_nothing() {
    let server = mock_model(good_draft()).await;
    let (h, admin) = setup(&server).await;
    let html = body(
        h.app
            .clone()
            .oneshot(get("/rules/draft?message=1", &admin))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("Compare with original"), "{html}");
    assert!(html.contains("Back to original"), "{html}");
    assert!(html.contains("Submit to AI"), "{html}");
    assert!(html.contains("‹CODE:dddd›"), "{html}");
    // The original is shown, the redaction is what the textarea holds.
    assert!(html.contains("Olá Ana Ferreira"), "{html}");
    assert!(
        html.contains("‹NAME_1›") && html.contains("‹ADDRESS_1›") && html.contains("‹PHONE_1›"),
        "{html}"
    );
    assert!(html.contains("test-model"), "{html}");
    assert!(server.received_requests().await.unwrap().is_empty());

    // One redaction action hides personal details and codes together.
    let html = body(
        h.app
            .clone()
            .oneshot(form(
                "/rules/draft",
                &admin,
                &[
                    ("_csrf", "csrf-token"),
                    ("message_id", "1"),
                    ("action", "redact"),
                ],
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("‹CODE:dddd›"), "{html}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn sending_drafts_a_rule_and_leaks_nothing() {
    let server = mock_model(good_draft()).await;
    let (h, admin) = setup(&server).await;
    // What the page proposes is what the admin sends.
    let page = body(
        h.app
            .clone()
            .oneshot(get("/rules/draft?message=1", &admin))
            .await
            .unwrap(),
    )
    .await;
    let start = page.find("Redacted message\">").unwrap() + "Redacted message\">".len();
    let redacted = html_unescape(&page[start..page[start..].find("</textarea>").unwrap() + start]);
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/rules/draft",
            &admin,
            &[
                ("_csrf", "csrf-token"),
                ("message_id", "1"),
                ("action", "send"),
                ("redacted", &redacted),
                ("hint", "Paack out for delivery, the 4 digits are the code"),
                (
                    "parts",
                    r#"[{"original":"LOCAL_ONLY_SECRET","text":"ignored","kind":"CUSTOM"}]"#,
                ),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body(res).await;
    // The editor opens with the draft, its explanation, and the real
    // message as the test message.
    assert!(html.contains("Drafted by"), "{html}");
    assert!(html.contains("test-model"), "{html}");
    assert!(
        html.contains("Paack&#39;s text on the day of delivery"),
        "{html}"
    );
    assert!(html.contains("(?i)entregue hoje"), "{html}");
    assert!(
        html.contains("Matches"),
        "the draft is checked against the original: {html}"
    );

    // Exactly one request, and none of the household's values were in it.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let sent = String::from_utf8(requests[0].body.clone()).unwrap();
    assert_eq!(find_leak(&sent, &household()), None, "{sent}");
    for secret in [
        "Ferreira",
        "Liberdade",
        "1250-096",
        "912 345 678",
        "ana@example.com",
    ] {
        assert!(!sent.contains(secret), "{secret} was sent: {sent}");
    }
    assert!(
        sent.contains("PCK7Q2W9D"),
        "the tracking number is what the rule reads"
    );
    assert!(sent.contains("the 4 digits are the code"));
    assert!(!sent.contains("5527"));
    assert!(!sent.contains("LOCAL_ONLY_SECRET"));
    assert!(
        requests[0].headers.get("authorization").is_none(),
        "no key configured"
    );

    // Saving keeps the real message as a redacted sample.
    let source = {
        let start = html.find("id=\"source\"").unwrap();
        let start = html[start..].find('>').unwrap() + start + 1;
        html_unescape(&html[start..html[start..].find("</textarea>").unwrap() + start])
    };
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/rules",
            &admin,
            &[
                ("_csrf", "csrf-token"),
                ("source", &source),
                ("test_text", ORIGINAL),
                ("test_channel", "share"),
                ("message_id", "1"),
                ("add_sample", "1"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER, "{}", body(res).await);
    let samples = h.db.blocking(|c| rule_db::all_samples(c)).unwrap();
    assert_eq!(samples.len(), 1);
    assert!(
        samples[0].text.contains("‹NAME_1›") && !samples[0].text.contains("Ferreira"),
        "{}",
        samples[0].text
    );
    assert_eq!(samples[0].expected["code"], "5527");
    // The waiting message became a package.
    assert_eq!(h.db.blocking(|c| messages::inbox(c)).unwrap().len(), 0);
}

fn html_unescape(s: &str) -> String {
    s.replace("&#60;", "<")
        .replace("&#62;", ">")
        .replace("&#38;", "&")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
}

#[tokio::test]
async fn a_household_value_typed_back_in_is_refused_before_anything_is_sent() {
    let server = mock_model(good_draft()).await;
    let (h, admin) = setup(&server).await;
    for (text, kind) in [
        ("Olá Ana Ferreira, encomenda PCK1AAAA1", "name"),
        (
            "encomenda PCK1AAAA1 para avenida da LIBERDADE 10",
            "address",
        ),
        ("encomenda PCK1AAAA1, tel +351 912-345-678", "phone"),
        ("encomenda PCK1AAAA1 ANA@example.com", "email"),
    ] {
        let res = h
            .app
            .clone()
            .oneshot(form(
                "/rules/draft",
                &admin,
                &[
                    ("_csrf", "csrf-token"),
                    ("message_id", "1"),
                    ("action", "send"),
                    ("redacted", text),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "{text}");
        let html = body(res).await;
        assert!(
            html.contains(&format!("still contains a household {kind}")),
            "{text}: {html}"
        );
    }
    // The hint is checked too.
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/rules/draft",
            &admin,
            &[
                ("_csrf", "csrf-token"),
                ("message_id", "1"),
                ("action", "send"),
                ("redacted", "encomenda PCK1AAAA1"),
                ("hint", "for David"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn drafting_is_for_admins_and_needs_a_model() {
    let server = mock_model(good_draft()).await;
    let (h, _) = setup(&server).await;
    let member = session_cookie(&h.key, &session(false));
    let res = h
        .app
        .clone()
        .oneshot(get("/rules/draft?message=1", &member))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/rules/draft",
            &member,
            &[
                ("_csrf", "csrf-token"),
                ("message_id", "1"),
                ("action", "send"),
                ("redacted", "x"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // Without --llm-model there is no page and no button.
    let plain = harness();
    let admin = session_cookie(&plain.key, &session(true));
    let res = plain
        .app
        .clone()
        .oneshot(get("/rules/draft?message=1", &admin))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let html = body(plain.app.oneshot(get("/inbox", &admin)).await.unwrap()).await;
    assert!(!html.contains("Draft with a model"), "{html}");
}

#[tokio::test]
async fn a_failing_model_is_reported_and_the_message_stays() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "Incorrect API key provided"}})),
        )
        .mount(&server)
        .await;
    let (h, admin) = setup(&server).await;
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/rules/draft",
            &admin,
            &[
                ("_csrf", "csrf-token"),
                ("message_id", "1"),
                ("action", "send"),
                ("redacted", "encomenda PCK1AAAA1"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_GATEWAY);
    let html = body(res).await;
    assert!(html.contains("Incorrect API key provided"), "{html}");
    assert_eq!(
        h.db.blocking(|c| messages::inbox(c)).unwrap()[0].result,
        MessageResult::Unmatched
    );
}

#[tokio::test]
async fn the_household_is_an_admin_setting() {
    let h = harness();
    let admin = session_cookie(&h.key, &session(true));
    let member = session_cookie(&h.key, &session(false));
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/settings/household",
            &member,
            &[("_csrf", "csrf-token"), ("names", "Ana")],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(
        h.db.blocking(|c| settings::household(c))
            .unwrap()
            .is_empty()
    );
    let html = body(
        h.app
            .clone()
            .oneshot(get("/settings", &member))
            .await
            .unwrap(),
    )
    .await;
    assert!(!html.contains("Household"), "members don't see it");

    let res = h
        .app
        .clone()
        .oneshot(form(
            "/settings/household",
            &admin,
            &[
                ("_csrf", "csrf-token"),
                ("names", "Ana Ferreira\n\n Ana \nAna"),
                ("phones", "912 345 678"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let saved = h.db.blocking(|c| settings::household(c)).unwrap();
    assert_eq!(saved.names, ["Ana Ferreira", "Ana"]);
    assert_eq!(saved.phones, ["912 345 678"]);
    let html = body(h.app.oneshot(get("/settings", &admin)).await.unwrap()).await;
    assert!(html.contains("Ana Ferreira"), "{html}");
}

const SHARED: &str = include_str!("fixtures/shared/paack-out-for-delivery.lastmile.yaml");

fn multipart(cookie: &str, text: &str, file: Option<&str>) -> Request<Body> {
    let boundary = "bndry";
    let mut body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"_csrf\"\r\n\r\ncsrf-token\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"text\"\r\n\r\n{text}\r\n"
    );
    if let Some(file) = file {
        body.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.lastmile.yaml\"\r\nContent-Type: text/yaml\r\n\r\n{file}\r\n"
        ));
    }
    body.push_str(&format!("--{boundary}--\r\n"));
    Request::post("/rules/import")
        .header(header::COOKIE, cookie)
        .header(header::ORIGIN, ORIGIN)
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn importing_previews_then_saves_the_rule_disabled() {
    let h = harness();
    let admin = session_cookie(&h.key, &session(true));
    // A waiting message the rule would read.
    h.db.blocking(|c| {
        ingest::ingest(
            c,
            &[],
            Incoming {
                channel: Channel::Share,
                submitted_by: None,
                sender: None,
                subject: None,
                sent_at: None,
                message_id: None,
                text: "Paack: encomenda PCK0ZZZZ9 será entregue hoje. Código de entrega 9090."
                    .into(),
                raw: b"x".to_vec(),
            },
            jiff::SignedDuration::from_hours(24),
            "2026-10-18T12:00:00Z".parse().unwrap(),
        )
    })
    .unwrap();
    let res = h
        .app
        .clone()
        .oneshot(multipart(&admin, SHARED, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body(res).await;
    assert!(html.contains("A new rule for Paack"), "{html}");
    assert!(html.contains("saved disabled"), "{html}");
    assert!(
        html.contains("PCK0ZZZZ9"),
        "the inbox message it would read: {html}"
    );
    assert_eq!(html.matches("passes").count(), 2, "both samples: {html}");
    assert!(
        h.db.blocking(|c| rule_db::list(c)).unwrap().is_empty(),
        "nothing saved by a preview"
    );

    let res = h
        .app
        .clone()
        .oneshot(form(
            "/rules/import/confirm",
            &admin,
            &[("_csrf", "csrf-token"), ("source", SHARED)],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        res.headers()[header::LOCATION],
        "/rules/9d2c4b7e-3f51-4a58-8f0e-6c1a2d7b9e40?notice=imported"
    );
    let rules = h.db.blocking(|c| rule_db::list(c)).unwrap();
    assert_eq!(rules.len(), 1);
    assert!(!rules[0].enabled);
    assert_eq!(rules[0].origin, rule_db::Origin::Imported);
    assert_eq!(h.db.blocking(|c| rule_db::all_samples(c)).unwrap().len(), 2);
    // Disabled: the waiting message is still waiting.
    assert_eq!(h.db.blocking(|c| messages::inbox(c)).unwrap().len(), 1);

    // Importing it again is an update, shown as a diff, and adds no samples twice.
    let changed = SHARED.replace("priority: 100", "priority: 120");
    let html = body(
        h.app
            .clone()
            .oneshot(multipart(&admin, "", Some(&changed)))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("Updates your rule"), "{html}");
    assert!(html.contains("priority: 120"), "{html}");
    h.app
        .clone()
        .oneshot(form(
            "/rules/import/confirm",
            &admin,
            &[("_csrf", "csrf-token"), ("source", &changed)],
        ))
        .await
        .unwrap();
    let rules = h.db.blocking(|c| rule_db::list(c)).unwrap();
    assert_eq!(
        (rules.len(), rules[0].version, rules[0].def.priority),
        (1, 2, 120)
    );
    assert_eq!(h.db.blocking(|c| rule_db::all_samples(c)).unwrap().len(), 2);
    // Enabling it turns the waiting message into a package.
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/rules/9d2c4b7e-3f51-4a58-8f0e-6c1a2d7b9e40/toggle",
            &admin,
            &[("_csrf", "csrf-token")],
        ))
        .await
        .unwrap();
    assert!(
        res.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .contains("resolved=1")
    );
}

#[tokio::test]
async fn bad_imports_are_refused_with_the_reasons() {
    let h = harness();
    let admin = session_cookie(&h.key, &session(true));
    let member = session_cookie(&h.key, &session(false));
    for (text, reason) in [
        ("hello", "YAML mapping"),
        ("slug: a", "not a lastmile rule"),
        ("lastmile: rule/v1\nid: nope\nslug: a", "UUID"),
        (
            &SHARED.replace("(?i)paack", "(?<=a)paack") as &str,
            "look-around",
        ),
        ("lastmile:rule:v1:AAAA", "does not unpack"),
    ] {
        let res = h
            .app
            .clone()
            .oneshot(multipart(&admin, text, None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY, "{text}");
        let html = body(res).await;
        assert!(html.contains(reason), "{text}: {html}");
    }
    // A sample that doesn't give what it says can't be imported, even if
    // someone posts the confirmation by hand.
    let wrong = SHARED.replace("'7315'", "'0000'");
    let html = body(
        h.app
            .clone()
            .oneshot(multipart(&admin, &wrong, None))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("fails"), "{html}");
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/rules/import/confirm",
            &admin,
            &[("_csrf", "csrf-token"), ("source", &wrong)],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    // Members can't import.
    let res = h
        .app
        .clone()
        .oneshot(multipart(&member, SHARED, None))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(h.db.blocking(|c| rule_db::list(c)).unwrap().is_empty());
}

#[tokio::test]
async fn a_rule_is_exported_with_redacted_samples_in_both_forms() {
    let h = harness();
    let admin = session_cookie(&h.key, &session(true));
    h.db.blocking(|c| settings::set_household(c, &household()))
        .unwrap();
    // A rule of our own with a sample holding personal data (kept as it is).
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/rules",
            &admin,
            &[
                ("_csrf", "csrf-token"),
                (
                    "source",
                    include_str!("fixtures/rules/paack-out-for-delivery.yaml"),
                ),
                ("test_text", ORIGINAL),
                ("test_channel", "share"),
                ("add_sample", "1"),
                ("keep_original", "1"),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let samples = h.db.blocking(|c| rule_db::all_samples(c)).unwrap();
    assert!(samples[0].text.contains("Ana Ferreira"), "kept as it is");
    let id = h.db.blocking(|c| rule_db::list(c)).unwrap()[0].id.clone();

    let html = body(
        h.app
            .clone()
            .oneshot(get(&format!("/rules/{id}/export"), &admin))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("lastmile: rule/v1"), "{html}");
    assert!(html.contains("lastmile:rule:v1:"), "{html}");
    assert!(html.contains("‹NAME_1›"), "{html}");
    for secret in [
        "Ferreira",
        "Liberdade",
        "1250-096",
        "912 345 678",
        "ana@example.com",
    ] {
        assert!(!html.contains(secret), "{secret} is in the export");
    }
    let res = h
        .app
        .clone()
        .oneshot(get(&format!("/rules/{id}/export.yaml"), &admin))
        .await
        .unwrap();
    assert_eq!(
        res.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"paack-out-for-delivery.lastmile.yaml\""
    );
    let file = body(res).await;
    // What was exported imports cleanly.
    let html = body(h.app.oneshot(multipart(&admin, &file, None)).await.unwrap()).await;
    assert!(html.contains("passes"), "{html}");
    assert!(!html.contains("fails"), "{html}");
}

#[tokio::test]
async fn redaction_preference_is_per_user_and_review_actions_never_send() {
    let server = mock_model(good_draft()).await;
    let (h, admin) = setup(&server).await;
    // CSRF is required even for a personal preference.
    let res = h
        .app
        .clone()
        .oneshot(form("/settings/preferences", &admin, &[]))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/settings/preferences",
            &admin,
            &[("_csrf", "csrf-token")],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        !h.db
            .blocking(|c| settings::redact_by_default(c, "u1"))
            .unwrap()
    );
    assert!(
        h.db.blocking(|c| settings::redact_by_default(c, "u2"))
            .unwrap()
    );
    let html = body(
        h.app
            .clone()
            .oneshot(get("/rules/draft?message=1", &admin))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("Submit original"));
    assert!(html.contains("Redact message"));
    assert!(!html.contains("‹CODE:dddd›"));
    for action in ["redact", "original"] {
        let html = body(
            h.app
                .clone()
                .oneshot(form(
                    "/rules/draft",
                    &admin,
                    &[
                        ("_csrf", "csrf-token"),
                        ("message_id", "1"),
                        ("action", action),
                    ],
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(html.contains("Back to original"), action == "redact");
        assert_eq!(html.contains("‹CODE:dddd›"), action == "redact");
        assert_eq!(html.contains("Submit original"), action == "original");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/settings/preferences",
            &admin,
            &[("_csrf", "csrf-token"), ("redact_by_default", "1")],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        h.db.blocking(|c| settings::redact_by_default(c, "u1"))
            .unwrap()
    );
}

#[tokio::test]
async fn revealed_household_values_are_blocked_and_custom_chips_survive_errors() {
    let server = mock_model(good_draft()).await;
    let (h, admin) = setup(&server).await;
    let text = "Ana Ferreira ‹CUSTOM_1›";
    let parts = serde_json::json!([
        {"original":"", "text":"Ana Ferreira ", "kind":""},
        {"original":"private note", "text":"‹CUSTOM_1›", "kind":"CUSTOM"}
    ])
    .to_string();
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/rules/draft",
            &admin,
            &[
                ("_csrf", "csrf-token"),
                ("message_id", "1"),
                ("action", "send"),
                ("is_redacted", "1"),
                ("redacted", text),
                ("parts", &parts),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let html = body(res).await;
    assert!(html.contains("Not sent:"));
    assert!(html.contains("data-original=\"private note\""));
    assert!(html.contains("‹CUSTOM_1›"));
    assert!(html.contains("Back to original"));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn redaction_styles_and_script_are_served() {
    let h = harness();
    for (path, expected) in [
        ("/assets/redaction.css", ".redaction-chip"),
        ("/assets/js/redaction.js", "data-hide-selection"),
    ] {
        let res = h.app.clone().oneshot(get(path, "")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(body(res).await.contains(expected));
    }
}
