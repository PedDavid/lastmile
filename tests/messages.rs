//! HTTP tests of the message channels (paste, upload, share, the ingest
//! API), the inbox and the rule editor.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

use common::*;
use lastmile::db::messages::{self, MessageResult};
use lastmile::db::{packages, rules as rule_db, tokens};
use lastmile::model::Status;

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

fn location(res: &axum::response::Response) -> String {
    res.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string()
}

const READY: &str = include_str!("fixtures/messages/ctt-ready-for-pickup.txt");
const READY_RULE: &str = include_str!("fixtures/rules/ctt-ready-for-pickup.yaml");

/// Saves a rule through the editor, as an admin.
async fn save_rule(h: &Harness, source: &str, extra: &[(&str, &str)]) -> axum::response::Response {
    let admin = session_cookie(&h.key, &session(true));
    let mut body = vec![("_csrf", "csrf-token"), ("source", source)];
    body.extend_from_slice(extra);
    h.app
        .clone()
        .oneshot(form("/rules", &admin, &body))
        .await
        .unwrap()
}

#[tokio::test]
async fn a_pasted_message_waits_until_a_rule_reads_it() {
    let h = harness();
    let member = session_cookie(&h.key, &session(false));
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/inbox/paste",
            &member,
            &[("_csrf", "csrf-token"), ("text", READY)],
        ))
        .await
        .unwrap();
    assert_eq!(location(&res), "/inbox?notice=unmatched");
    let html = body(h.app.clone().oneshot(get("/inbox", &member)).await.unwrap()).await;
    assert!(html.contains("RR123456789PT"), "{html}");
    assert!(html.contains("No rule"), "{html}");
    assert!(!html.contains("Build rule"), "members can't write rules");

    // An admin writes the rule, keeping a test message as its sample; the
    // waiting message becomes a package.
    let res = save_rule(
        &h,
        READY_RULE,
        &[
            ("test_text", READY),
            ("test_channel", "share"),
            ("add_sample", "1"),
        ],
    )
    .await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let to = location(&res);
    assert!(to.ends_with("?notice=saved&resolved=1"), "{to}");
    let p = h.db.blocking(|c| packages::get(c, 1)).unwrap().unwrap();
    assert_eq!(p.status, Status::ReadyForPickup);
    assert_eq!(p.code.as_deref(), Some("482913"));
    assert_eq!(
        p.pickup_point.name.as_deref(),
        Some("Cacifo CTT Mercado de Benfica")
    );
    assert!(h.db.blocking(|c| messages::inbox(c)).unwrap().is_empty());
    assert_eq!(h.db.blocking(|c| rule_db::all_samples(c)).unwrap().len(), 1);

    // The next one goes straight to the package.
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/inbox/paste",
            &member,
            &[("_csrf", "csrf-token"), ("text", READY)],
        ))
        .await
        .unwrap();
    assert_eq!(location(&res), "/packages/1?notice=updated-from-message");
    let html = body(h.app.oneshot(get("/packages/1", &member)).await.unwrap()).await;
    assert!(html.contains("Messages"), "{html}");
}

#[tokio::test]
async fn rules_are_for_admins() {
    let h = harness();
    let member = session_cookie(&h.key, &session(false));
    for uri in ["/rules", "/rules/new"] {
        let res = h.app.clone().oneshot(get(uri, &member)).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN, "{uri}");
    }
    let res = h
        .app
        .clone()
        .oneshot(form(
            "/rules",
            &member,
            &[("_csrf", "csrf-token"), ("source", READY_RULE)],
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_editor_previews_and_refuses_broken_rules() {
    let h = harness();
    let admin = session_cookie(&h.key, &session(true));
    let html = body(
        h.app
            .clone()
            .oneshot(form(
                "/rules/preview",
                &admin,
                &[
                    ("_csrf", "csrf-token"),
                    ("source", READY_RULE),
                    ("test_text", READY),
                ],
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("Matches"), "{html}");
    assert!(html.contains("482913"), "{html}");

    let broken = "slug: x\nname: X\nmatch: {all: ['(?<=a)b']}\n";
    let html = body(
        h.app
            .clone()
            .oneshot(form(
                "/rules/preview",
                &admin,
                &[("_csrf", "csrf-token"), ("source", broken)],
            ))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("look-around is not supported"), "{html}");
    let res = save_rule(&h, broken, &[]).await;
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(h.db.blocking(|c| rule_db::list(c)).unwrap().is_empty());
}

#[tokio::test]
async fn a_rule_that_takes_another_rules_sample_needs_confirming() {
    let h = harness();
    let res = save_rule(
        &h,
        READY_RULE,
        &[
            ("test_text", READY),
            ("test_channel", "share"),
            ("add_sample", "1"),
        ],
    )
    .await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    // A broader rule with a higher priority would now win that sample.
    let greedy = "slug: greedy\nname: Greedy\npriority: 500\nmatch: {all: ['CTT']}\n\
                  extract: {tracking_number: '(?P<v>RR\\d+PT)'}\n";
    let res = save_rule(&h, greedy, &[]).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let html = body(res).await;
    assert!(html.contains("Save anyway"), "{html}");
    assert_eq!(h.db.blocking(|c| rule_db::list(c)).unwrap().len(), 1);
    let res = save_rule(&h, greedy, &[("confirm", "1")]).await;
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(h.db.blocking(|c| rule_db::list(c)).unwrap().len(), 2);
}

fn multipart(cookie: &str, files: &[(&str, &[u8])], csrf: &str) -> Request<Body> {
    let boundary = "XyZboundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"_csrf\"\r\n\r\n{csrf}\r\n")
            .as_bytes(),
    );
    for (name, content) in files {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{name}\"\r\nContent-Type: message/rfc822\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(content);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Request::post("/inbox/upload")
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
async fn emails_are_uploaded_once() {
    let h = harness();
    save_rule(&h, include_str!("fixtures/rules/ctt-in-transit.yaml"), &[]).await;
    let member = session_cookie(&h.key, &session(false));
    let transit: &[u8] = include_bytes!("fixtures/messages/ctt-in-transit.eml");
    let shop: &[u8] = include_bytes!("fixtures/messages/wook-order.eml");
    let res = h
        .app
        .clone()
        .oneshot(multipart(
            &member,
            &[
                ("transit.eml", transit),
                ("shop.eml", shop),
                ("photo.png", b"\x89PNG"),
            ],
            "csrf-token",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body(res).await;
    assert!(html.contains("CTT RR987654321PT – in transit"), "{html}");
    assert!(html.contains("No rule matched – in the inbox"), "{html}");
    assert!(html.contains("not an email"), "{html}");
    let p = h.db.blocking(|c| packages::get(c, 1)).unwrap().unwrap();
    assert_eq!(p.eta_from, Some("2026-10-21T00:00:00Z".parse().unwrap()));

    let html = body(
        h.app
            .clone()
            .oneshot(multipart(&member, &[("again.eml", transit)], "csrf-token"))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("Already received"), "{html}");
    // The CSRF token travels in the multipart body.
    let res = h
        .app
        .oneshot(multipart(&member, &[("x.eml", shop)], "wrong"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

fn api(token: Option<&str>, body: &str) -> Request<Body> {
    let mut req = Request::post("/api/ingest").header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    req.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn the_shortcut_sends_text_with_a_token() {
    let h = harness();
    save_rule(&h, READY_RULE, &[]).await;
    let member = session_cookie(&h.key, &session(false));
    let html = body(
        h.app
            .clone()
            .oneshot(form(
                "/settings/tokens",
                &member,
                &[("_csrf", "csrf-token"), ("name", "iPhone")],
            ))
            .await
            .unwrap(),
    )
    .await;
    let start = html.find("lm_").expect("the token is shown once");
    let token: String = html[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    // ...and only once.
    let html = body(
        h.app
            .clone()
            .oneshot(get("/settings", &member))
            .await
            .unwrap(),
    )
    .await;
    assert!(!html.contains(&token));
    assert!(html.contains("iPhone"));

    let json = serde_json::json!({ "text": READY }).to_string();
    let res = h.app.clone().oneshot(api(None, &json)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let res = h
        .app
        .clone()
        .oneshot(api(Some("lm_forged"), &json))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    // No Origin and no cookie needed: the token is the credential.
    let res = h
        .app
        .clone()
        .oneshot(api(Some(&token), &json))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let reply: serde_json::Value = serde_json::from_str(&body(res).await).unwrap();
    assert_eq!(
        reply["summary"],
        "CTT RR123456789PT – ready for pickup, code 482913"
    );
    assert_eq!(reply["package"]["url"], "http://localhost:8080/packages/1");
    let m = h.db.blocking(|c| messages::get(c, 1)).unwrap().unwrap();
    assert_eq!(m.result, MessageResult::Matched);
    assert_eq!(m.submitted_by.as_deref(), Some("rui"));

    let res = h
        .app
        .clone()
        .oneshot(api(Some(&token), "{\"text\": \"\"}"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let id = h.db.blocking(|c| tokens::list(c, "u2")).unwrap()[0].id;
    h.app
        .clone()
        .oneshot(form(
            &format!("/settings/tokens/{id}/revoke"),
            &member,
            &[("_csrf", "csrf-token")],
        ))
        .await
        .unwrap();
    let res = h.app.oneshot(api(Some(&token), &json)).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn shared_text_is_confirmed_before_it_is_stored() {
    let h = harness();
    let member = session_cookie(&h.key, &session(false));
    let res = h
        .app
        .clone()
        .oneshot(get(
            "/share?title=CTT&text=Encomenda%20RR123456789PT",
            &member,
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let html = body(res).await;
    assert!(html.contains("Encomenda RR123456789PT"), "{html}");
    assert!(html.contains(r#"name="channel" value="share""#), "{html}");
    assert!(
        h.db.blocking(|c| messages::inbox(c)).unwrap().is_empty(),
        "nothing stored yet"
    );
}

#[tokio::test]
async fn inbox_messages_are_attached_or_ignored() {
    let app = demo_app();
    let html = body(
        app.clone()
            .oneshot(Request::get("/inbox").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("Which package?"), "{html}");
    assert!(html.contains("Rule failed"), "{html}");
    assert!(
        html.contains("Confirmação da encomenda W-2026-339120"),
        "{html}"
    );
    let demo = |uri: &str, body: &str| {
        Request::post(uri)
            .header(header::ORIGIN, "http://localhost:8080")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    // Demo messages: 1 is linked to the shoes; then the reminder (2), the
    // notice without a number (3), the shop's email (4) and Paack's (5).
    let res = app
        .clone()
        .oneshot(demo(
            "/inbox/2/attach",
            "_csrf=demo-csrf-token&package_id=1",
        ))
        .await
        .unwrap();
    assert_eq!(location(&res), "/packages/1?notice=attached");
    let res = app
        .clone()
        .oneshot(demo("/inbox/5/ignore", "_csrf=demo-csrf-token"))
        .await
        .unwrap();
    assert_eq!(location(&res), "/inbox?notice=ignored");
    let html = body(
        app.oneshot(Request::get("/inbox").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    assert!(!html.contains("Which package?"), "{html}");
    assert!(!html.contains("Paack: Olá!"), "{html}");
}

#[tokio::test]
async fn a_package_can_be_created_from_a_message() {
    let app = demo_app();
    // Message 3 is the pickup notice without a tracking number.
    let html = body(
        app.clone()
            .oneshot(
                Request::get("/packages/new?message=3")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains(r#"value="771204""#), "{html}");
    assert!(html.contains(r#"name="message_id" value="3""#), "{html}");
    assert!(html.contains(r#"value="Loja CTT Restauradores""#), "{html}");
}

#[tokio::test]
async fn the_manifest_makes_it_a_share_target() {
    let app = demo_app();
    let res = app
        .clone()
        .oneshot(
            Request::get("/manifest.webmanifest")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.headers()[header::CONTENT_TYPE],
        "application/manifest+json"
    );
    let manifest: serde_json::Value = serde_json::from_str(&body(res).await).unwrap();
    assert_eq!(manifest["share_target"]["action"], "/share");
    assert_eq!(manifest["share_target"]["method"], "GET");
    let res = app
        .oneshot(Request::get("/sw.js").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CACHE_CONTROL], "no-cache");
}
