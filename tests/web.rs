//! HTTP-level tests of the UI router: auth, Origin checks, headers and pages.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

use common::*;
use lastmile::db::packages;
use lastmile::model::NewPackage;

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
    let ops = lastmile::web::ops_router(h.db);
    for path in ["/healthz", "/readyz"] {
        let res = ops
            .clone()
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK, "{path}");
    }
}
