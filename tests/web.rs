//! HTTP-level tests of the UI router: auth, Origin checks, headers and pages.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

use common::*;
use lastmile::db::packages;
use lastmile::model::{NewPackage, Source, Status};

#[tokio::test]
async fn unauthenticated_users_are_sent_to_login() {
    let h = harness();
    let res = h
        .app
        .oneshot(Request::get("/?x=1").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        res.headers()[header::LOCATION],
        "/auth/login?next=%2F%3Fx%3D1"
    );
}

#[tokio::test]
async fn htmx_requests_get_hx_redirect() {
    let h = harness();
    let res = h
        .app
        .oneshot(
            Request::get("/search?q=x")
                .header("hx-request", "true")
                .header("hx-current-url", "http://localhost:8080/")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(res.headers()["hx-redirect"], "/auth/login?next=%2F");
}

#[tokio::test]
async fn logged_in_users_see_the_packages() {
    let h = harness();
    let now = NOW.parse().unwrap();
    h.db.blocking(|c| packages::insert(c, &NewPackage::titled("Shoes <b>", now), "Added", now))
        .unwrap();
    let cookie = session_cookie(&h.key, &session(false));
    let res = h
        .app
        .oneshot(
            Request::get("/")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let headers = res.headers().clone();
    let html = body(res).await;
    // Escaped, never rendered as markup.
    assert!(html.contains("Shoes &#60;b&#62;"), "{html}");
    assert!(html.contains("rui"));
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert!(
        headers[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .contains("script-src 'self'")
    );
}

#[tokio::test]
async fn cross_origin_posts_are_rejected() {
    let h = harness();
    let cookie = session_cookie(&h.key, &session(true));
    let res = h
        .app
        .oneshot(
            Request::post("/auth/logout")
                .header(header::COOKIE, cookie)
                .header(header::ORIGIN, "https://evil.example")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("_csrf=csrf-token"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn logout_needs_the_csrf_token() {
    let h = harness();
    let cookie = session_cookie(&h.key, &session(true));
    let post = |csrf: &str| {
        Request::post("/auth/logout")
            .header(header::COOKIE, cookie.clone())
            .header(header::ORIGIN, ORIGIN)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(format!("_csrf={csrf}")))
            .unwrap()
    };
    let res = h.app.clone().oneshot(post("nope")).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = h.app.oneshot(post("csrf-token")).await.unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn the_palette_searches_titles_and_tracking_numbers() {
    let app = demo_app();
    let get = |q: &str| {
        Request::get(format!("/search?q={q}"))
            .body(Body::empty())
            .unwrap()
    };
    let html = body(app.clone().oneshot(get("RR1234")).await.unwrap()).await;
    assert!(html.contains("Trail running shoes"), "{html}");
    assert!(!html.contains("Phone case"), "{html}");
    // Archived packages are found too.
    let html = body(app.oneshot(get("lamp")).await.unwrap()).await;
    assert!(html.contains("Desk lamp"), "{html}");
}

#[tokio::test]
async fn health_and_readiness() {
    let h = harness();
    let ops = lastmile::web::ops_router(h.db.clone(), lastmile::metrics::Metrics::new(h.db));
    for path in ["/healthz", "/readyz"] {
        let res = ops
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
    }
}

fn form_post(uri: &str, cookie: &str, body: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::COOKIE, cookie)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get(uri: &str, cookie: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

#[tokio::test]
async fn household_members_add_edit_and_archive_packages() {
    let h = harness();
    let cookie = session_cookie(&h.key, &session(false));
    let res = h
        .app
        .clone()
        .oneshot(form_post(
            "/packages",
            &cookie,
            "_csrf=csrf-token&title=Shoes&tracking_number=RR123456789PT&code=482913&code_kind=pickup&pickup_name=Locker",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(res.headers()[header::LOCATION], "/packages/1?notice=added");
    let p = h.db.blocking(|c| packages::get(c, 1)).unwrap().unwrap();
    assert_eq!(
        p.courier.as_deref(),
        Some("CTT"),
        "detected from the number"
    );

    let html = body(
        h.app
            .clone()
            .oneshot(get("/packages/1", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("482913"), "{html}");
    assert!(html.contains("Added by rui"), "{html}");

    let res = h
        .app
        .clone()
        .oneshot(form_post(
            "/packages/1",
            &cookie,
            "_csrf=csrf-token&title=Trail+shoes",
        ))
        .await
        .unwrap();
    assert_eq!(res.headers()[header::LOCATION], "/packages/1?notice=saved");
    let p = h.db.blocking(|c| packages::get(c, 1)).unwrap().unwrap();
    assert_eq!(p.title, "Trail shoes");
    assert_eq!(p.code, None, "cleared fields are cleared");

    let res = h
        .app
        .clone()
        .oneshot(form_post(
            "/packages/1/status",
            &cookie,
            "_csrf=csrf-token&status=delivered&note=Collected",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    let p = h.db.blocking(|c| packages::get(c, 1)).unwrap().unwrap();
    assert_eq!(p.status, Status::Delivered);
    assert_eq!(p.status_source, Source::Manual);
    let events = h.db.blocking(|c| packages::events(c, 1)).unwrap();
    assert_eq!(events[0].description, "Collected (set by rui)");

    h.app
        .clone()
        .oneshot(form_post(
            "/packages/1/archive",
            &cookie,
            "_csrf=csrf-token",
        ))
        .await
        .unwrap();
    let p = h.db.blocking(|c| packages::get(c, 1)).unwrap().unwrap();
    assert!(p.archived_at.is_some());
    // Archived packages leave the default list but not the archive.
    let html = body(h.app.clone().oneshot(get("/", &cookie)).await.unwrap()).await;
    assert!(!html.contains("Trail shoes"), "{html}");
    let html = body(
        h.app
            .clone()
            .oneshot(get("/?show=archived", &cookie))
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("Trail shoes"), "{html}");

    // Deleting removes it entirely, history included.
    let res = h
        .app
        .clone()
        .oneshot(form_post("/packages/1/delete", &cookie, "_csrf=csrf-token"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(res.headers()[header::LOCATION], "/");
    assert!(h.db.blocking(|c| packages::get(c, 1)).unwrap().is_none());
    assert!(
        h.db.blocking(|c| packages::events(c, 1))
            .unwrap()
            .is_empty()
    );
    let res = h
        .app
        .oneshot(form_post("/packages/1/delete", &cookie, "_csrf=csrf-token"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invalid_forms_are_shown_again_with_the_error() {
    let h = harness();
    let cookie = session_cookie(&h.key, &session(false));
    let res = h
        .app
        .clone()
        .oneshot(form_post(
            "/packages",
            &cookie,
            "_csrf=csrf-token&title=&merchant=Wook",
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let html = body(res).await;
    assert!(html.contains("Give the package a name."), "{html}");
    assert!(html.contains(r#"value="Wook""#), "typed values are kept");
    // And without the CSRF token, nothing is written.
    let res = h
        .app
        .oneshot(form_post("/packages", &cookie, "title=Shoes"))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(h.db.blocking(|c| packages::get(c, 1)).unwrap().is_none());
}

#[tokio::test]
async fn filters_and_their_counts() {
    let app = demo_app();
    let html = body(
        app.clone()
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    // Six active packages; the archived lamp is only counted under Archived.
    assert!(
        html.contains(r#"All <span class="text-muted-foreground">6</span>"#),
        "{html}"
    );
    let html = body(
        app.clone()
            .oneshot(
                Request::get("/?show=attention")
                    .header("hx-request", "true")
                    .header("hx-target", "packages")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("Kitchen scale"), "{html}");
    assert!(html.contains("Problem reported"), "{html}");
    assert!(!html.contains("Coffee beans"), "{html}");
    assert!(
        html.contains(r#"hx-swap-oob="true""#),
        "chips are refreshed too"
    );
    let html = body(
        app.oneshot(Request::get("/?recipient=Ana").body(Body::empty()).unwrap())
            .await
            .unwrap(),
    )
    .await;
    assert!(html.contains("Books (3)"), "{html}");
    assert!(!html.contains("Coffee beans"), "{html}");
}

#[tokio::test]
async fn metrics_count_packages() {
    let h = harness();
    let now = NOW.parse().unwrap();
    h.db.blocking(|c| lastmile::demo::load(c, now)).unwrap();
    let ops = lastmile::web::ops_router(h.db.clone(), lastmile::metrics::Metrics::new(h.db));
    let res = ops
        .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let text = body(res).await;
    assert!(
        text.contains(r#"lastmile_packages{status="out_for_delivery",courier="Paack"} 1"#),
        "{text}"
    );
}
