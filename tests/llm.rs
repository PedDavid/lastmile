//! The model that drafts rules (a mock OpenAI-style server), what is sent to
//! it, and sharing rules.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use common::*;
use lastmile::db::messages::MessageResult;
use lastmile::db::{messages, rules as rule_db};
use lastmile::ingest::{self, Incoming};
use lastmile::model::Channel;

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

/// An app with an unmatched message in the inbox and a model.
async fn setup(server: &MockServer) -> (Harness, String) {
    let h = harness_llm(&format!("{}/v1", server.uri()));
    let admin = session_cookie(&h.key, &session(true));
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
    assert!(html.contains("What is sent"), "{html}");
    // The message, as it is, is what the textarea holds.
    assert!(html.contains("Olá Ana Ferreira"), "{html}");
    assert!(html.contains("test-model"), "{html}");
    assert!(server.received_requests().await.unwrap().is_empty());

    // "Reset" throws the edits away and sends nothing either.
    let html = body(
        h.app
            .clone()
            .oneshot(form(
                "/rules/draft",
                &admin,
                &[
                    ("_csrf", "csrf-token"),
                    ("message_id", "1"),
                    ("action", "reset"),
                    ("text", "edited"),
                ],
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("Olá Ana Ferreira"), "{html}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn sending_drafts_a_rule_from_the_text_the_admin_read() {
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
    let start = page.find("Text to send\">").unwrap() + "Text to send\">".len();
    let text = html_unescape(&page[start..page[start..].find("</textarea>").unwrap() + start]);
    assert_eq!(text, ORIGINAL);
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
                ("text", &text),
                ("hint", "Paack out for delivery, the 4 digits are the code"),
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
        "the draft is checked against the message: {html}"
    );

    // Exactly one request, holding the text that was read and the hint.
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let sent = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(sent.contains("PCK7Q2W9D"), "{sent}");
    assert!(sent.contains("the 4 digits are the code"));
    assert!(
        requests[0].headers.get("authorization").is_none(),
        "no key configured"
    );

    // Saving keeps the message as the rule's sample.
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
    assert_eq!(samples[0].text, ORIGINAL);
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
async fn only_the_text_the_admin_left_is_sent() {
    let server = mock_model(good_draft()).await;
    let (h, admin) = setup(&server).await;
    // Personal parts edited out before sending.
    let edited = "Paack: Olá, a sua encomenda PCK7Q2W9D da Worten será entregue hoje. Código de entrega 5527.";
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
                ("text", edited),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let sent = String::from_utf8(requests[0].body.clone()).unwrap();
    assert!(sent.contains(edited), "{sent}");
    for removed in ["Ferreira", "Liberdade", "912 345 678", "ana@example.com"] {
        assert!(!sent.contains(removed), "{removed} was sent: {sent}");
    }
    // The message in the inbox is untouched until the rule is saved.
    assert_eq!(h.db.blocking(|c| messages::inbox(c)).unwrap().len(), 1);

    // Nothing to send, nothing sent.
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
                ("text", "  \r\n "),
            ],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
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
                ("text", "x"),
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
                ("text", "encomenda PCK1AAAA1"),
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
async fn a_rule_is_exported_with_its_samples_in_both_forms() {
    let h = harness();
    let admin = session_cookie(&h.key, &session(true));
    // A rule of our own with a sample holding personal data.
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
    // The samples are there as saved, and the page says so.
    assert!(html.contains("Ana Ferreira"), "{html}");
    assert!(html.contains("share the rule without them"), "{html}");
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
    let html = body(
        h.app
            .clone()
            .oneshot(multipart(&admin, &file, None))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("passes"), "{html}");
    assert!(!html.contains("fails"), "{html}");

    // Without the samples, the rule goes alone.
    let alone = body(
        h.app
            .clone()
            .oneshot(get(
                &format!("/rules/{id}/export.yaml?samples=none"),
                &admin,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(
        !alone.contains("Ferreira") && !alone.contains("samples:"),
        "{alone}"
    );
    let html = body(
        h.app
            .oneshot(get(&format!("/rules/{id}/export?samples=none"), &admin))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("Shared without its samples"), "{html}");
    assert!(!html.contains("Ferreira"), "{html}");
}
