//! HTTP layer: the UI router, security middleware, static assets and the
//! separate metrics/health router.

mod api;
pub mod auth;
mod draft;
mod forms;
mod inbox;
mod pages;
mod rules;
mod settings;
mod share;
mod views;

pub use forms::parse_date;
pub use views::safe_link;

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use askama::Template;
use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, FromRef, Path, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum_extra::extract::cookie::Key;
use jiff::Timestamp;
use rust_embed::RustEmbed;
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::db::Db;
use crate::metrics::Metrics;
use auth::AuthMode;

#[derive(Clone)]
pub struct AppState {
    pub inner: Arc<Inner>,
}

pub struct Inner {
    pub cfg: Config,
    pub db: Db,
    pub metrics: Arc<Metrics>,
    /// The model that drafts rules; `None` leaves the feature off.
    pub llm: Option<crate::llm::Client>,
    pub auth: AuthMode,
    pub rate_limiter: api::RateLimiter,
    pub cookie_key: Key,
    pub asset_version: String,
    /// Pins the clock the pages render relative dates against (tests only).
    pub fixed_now: Option<Timestamp>,
}

impl AppState {
    pub fn new(cfg: Config, db: Db, auth: AuthMode, cookie_key: Key) -> Self {
        AppState {
            inner: Arc::new(Inner {
                cfg,
                metrics: Metrics::new(db.clone()),
                llm: None,
                db,
                auth,
                rate_limiter: api::RateLimiter::default(),
                cookie_key,
                asset_version: asset_version(),
                fixed_now: None,
            }),
        }
    }

    /// Turns rule drafting on with this client.
    pub fn with_llm(mut self, llm: Option<crate::llm::Client>) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("the LLM is set before the state is shared")
            .llm = llm;
        self
    }

    /// Renders every page as if it were `now`, so output is reproducible.
    /// Sessions keep the real clock.
    pub fn with_fixed_now(mut self, now: Timestamp) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("fixed_now is set before the state is shared")
            .fixed_now = Some(now);
        self
    }

    fn now(&self) -> Timestamp {
        self.inner.fixed_now.unwrap_or_else(Timestamp::now)
    }
}

impl FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Key {
        state.inner.cookie_key.clone()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Upstream(String),
    #[error("{0}")]
    Internal(String),
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    status: u16,
    title: &'a str,
    message: &'a str,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, title) = match &self {
            AppError::NotFound(_) => (StatusCode::NOT_FOUND, "Not found"),
            AppError::BadRequest(_) => (StatusCode::BAD_REQUEST, "Bad request"),
            AppError::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "Login failed"),
            AppError::Forbidden(_) => (StatusCode::FORBIDDEN, "Forbidden"),
            AppError::Upstream(_) => (StatusCode::BAD_GATEWAY, "Upstream error"),
            AppError::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong"),
        };
        if status.is_server_error() {
            tracing::error!(error = %self, "request failed");
        }
        let message = self.to_string();
        let page = ErrorPage {
            status: status.as_u16(),
            title,
            message: &message,
        };
        match page.render() {
            Ok(html) => (status, Html(html)).into_response(),
            Err(_) => (status, message).into_response(),
        }
    }
}

impl From<crate::db::DbError> for AppError {
    fn from(e: crate::db::DbError) -> Self {
        AppError::Internal(e.to_string())
    }
}

impl From<askama::Error> for AppError {
    fn from(e: askama::Error) -> Self {
        AppError::Internal(format!("template error: {e}"))
    }
}

#[derive(RustEmbed)]
#[folder = "assets/"]
#[include = "dist/*"]
#[include = "js/*"]
#[include = "vendor/htmx/*.js"]
#[include = "icons/*"]
#[include = "manifest.webmanifest"]
struct Assets;

fn asset_version() -> String {
    let mut hasher = DefaultHasher::new();
    for file in Assets::iter() {
        if let Some(content) = Assets::get(&file) {
            content.data.hash(&mut hasher);
        }
    }
    format!("{:x}", hasher.finish() & 0xffff_ffff)
}

async fn asset(Path(path): Path<String>) -> Response {
    serve_asset(&path, "public, max-age=31536000, immutable")
}

/// The service worker and the manifest live at fixed URLs (the worker's
/// scope is its directory), so they are revalidated instead of cached for
/// good.
async fn service_worker() -> Response {
    let mut res = serve_asset("js/sw.js", "no-cache");
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/javascript"),
    );
    res
}

async fn manifest() -> Response {
    let mut res = serve_asset("manifest.webmanifest", "no-cache");
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/manifest+json"),
    );
    res
}

fn serve_asset(path: &str, cache: &'static str) -> Response {
    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, mime.as_ref().to_string()),
                    (header::CACHE_CONTROL, cache.to_string()),
                ],
                Body::from(content.data.into_owned()),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
connect-src 'self'; font-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

/// Security headers on every response; `no-store` on everything but assets.
async fn security_headers(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let is_asset =
        path.starts_with("/assets/") || path == "/sw.js" || path == "/manifest.webmanifest";
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("same-origin"),
    );
    if !is_asset {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

/// Rejects state-changing requests whose `Origin` is not the public URL.
/// The ingest API is exempt: it takes bearer tokens only, never cookies.
async fn origin_check(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
        || req.uri().path() == "/api/ingest";
    if !safe {
        let origin = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok());
        if origin != Some(state.inner.cfg.public_origin().as_str()) {
            return AppError::Forbidden(format!(
                "cross-origin request rejected (Origin {origin:?}, expected {})",
                state.inner.cfg.public_origin()
            ))
            .into_response();
        }
    }
    next.run(req).await
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(pages::index))
        .route("/search", get(pages::search))
        .route("/packages", post(forms::create))
        .route("/packages/new", get(forms::new))
        .route("/packages/{id}", get(pages::detail).post(forms::update))
        .route("/packages/{id}/edit", get(forms::edit))
        .route("/packages/{id}/status", post(forms::set_status))
        .route("/packages/{id}/archive", post(forms::archive))
        .route("/packages/{id}/unarchive", post(forms::unarchive))
        .route("/inbox", get(inbox::inbox))
        .route("/inbox/paste", post(inbox::paste))
        .route(
            "/inbox/upload",
            post(inbox::upload).layer(DefaultBodyLimit::max(
                inbox::MAX_FILES * inbox::MAX_FILE_BYTES + 64 * 1024,
            )),
        )
        .route("/inbox/{id}/attach", post(inbox::attach))
        .route("/inbox/{id}/ignore", post(inbox::ignore))
        .route("/share", get(inbox::share))
        .route("/rules", get(rules::list).post(rules::create))
        .route("/rules/new", get(rules::new))
        .route("/rules/draft", get(draft::form).post(draft::draft))
        .route("/rules/import", get(share::import_form).post(share::import_preview))
        .route("/rules/import/confirm", post(share::import_confirm))
        .route("/rules/preview", post(rules::preview_new))
        .route("/rules/{id}", get(rules::edit).post(rules::update))
        .route("/rules/{id}/preview", post(rules::preview_existing))
        .route("/rules/{id}/toggle", post(rules::toggle))
        .route("/rules/{id}/export", get(share::export_page))
        .route("/rules/{id}/export.yaml", get(share::export_file))
        .route(
            "/rules/{id}/samples/{sample}/delete",
            post(rules::delete_sample),
        )
        .route("/settings", get(settings::settings))
        .route("/settings/household", post(settings::save_household))
        .route("/settings/tokens", post(settings::create_token))
        .route("/settings/tokens/{id}/revoke", post(settings::revoke_token))
        .route(
            "/api/ingest",
            post(api::ingest_api).layer(DefaultBodyLimit::max(api::MAX_BODY)),
        )
        .route("/sw.js", get(service_worker))
        .route("/manifest.webmanifest", get(manifest))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        .route("/assets/{*path}", get(asset))
        .fallback(|| async { AppError::NotFound("page not found".into()) })
        .layer(middleware::from_fn_with_state(state.clone(), origin_check))
        .layer(middleware::from_fn(security_headers))
        .layer(DefaultBodyLimit::max(256 * 1024))
        // Spans record method and path only: no query strings (the OIDC
        // callback carries the authorization code, and /share the shared
        // text), headers or bodies.
        .layer(TraceLayer::new_for_http().make_span_with(|req: &Request| {
            tracing::info_span!("http", method = %req.method(), path = %req.uri().path())
        }))
        .with_state(state)
}

/// `/metrics`, `/healthz` and `/readyz`, served on their own port.
pub fn ops_router(db: Db, metrics: Arc<Metrics>) -> Router {
    Router::new()
        .route(
            "/metrics",
            get(move || {
                let metrics = metrics.clone();
                async move {
                    // The collector queries the database.
                    match tokio::task::spawn_blocking(move || metrics.encode()).await {
                        Ok(text) => (
                            [(
                                header::CONTENT_TYPE,
                                "application/openmetrics-text; version=1.0.0; charset=utf-8",
                            )],
                            text,
                        )
                            .into_response(),
                        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                }
            }),
        )
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/readyz",
            get(move || {
                let db = db.clone();
                async move {
                    if db.ping().await {
                        (StatusCode::OK, "ready")
                    } else {
                        (StatusCode::SERVICE_UNAVAILABLE, "database unavailable")
                    }
                }
            }),
        )
}
