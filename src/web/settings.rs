//! Settings → Devices: each person's ingest tokens for the iOS Shortcut.

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use jiff::Timestamp;
use serde::Deserialize;

use super::auth::{Session, User};
use super::pages::Layout;
use super::views::relative;
use super::{AppError, AppState};
use crate::db::tokens;

pub struct TokenRow {
    pub id: i64,
    pub name: String,
    pub created: String,
    pub last_used: String,
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsPage {
    layout: Layout,
    tokens: Vec<TokenRow>,
    /// A token just created: shown once, never again.
    new_token: String,
    ingest_url: String,
}

async fn page(
    state: &AppState,
    session: &Session,
    jar: &CookieJar,
    new_token: String,
) -> Result<Response, AppError> {
    let now = state.now();
    let sub = session.sub.clone();
    let list = state.inner.db.call(move |c| tokens::list(c, &sub)).await?;
    let tokens = list
        .iter()
        .map(|t| TokenRow {
            id: t.id,
            name: t.name.clone(),
            created: relative(t.created_at, now),
            last_used: t
                .last_used_at
                .map(|t| relative(t, now))
                .unwrap_or_else(|| "never".into()),
        })
        .collect();
    Ok(Html(
        SettingsPage {
            layout: Layout::new(state, session, jar, "Settings"),
            tokens,
            new_token,
            ingest_url: format!(
                "{}/api/ingest",
                state.inner.cfg.public_url.as_str().trim_end_matches('/')
            ),
        }
        .render()?,
    )
    .into_response())
}

pub async fn settings(
    State(state): State<AppState>,
    User(session): User,
    jar: CookieJar,
) -> Result<Response, AppError> {
    page(&state, &session, &jar, String::new()).await
}

#[derive(Deserialize)]
pub struct NewToken {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
    name: String,
}

pub async fn create_token(
    State(state): State<AppState>,
    User(session): User,
    jar: CookieJar,
    headers: HeaderMap,
    Form(form): Form<NewToken>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let name = form.name.trim().to_string();
    if name.is_empty() || name.len() > 100 {
        return Err(AppError::BadRequest(
            "name the device (up to 100 characters)".into(),
        ));
    }
    let secret = tokens::generate();
    let (sub, user, value) = (session.sub.clone(), session.name.clone(), secret.clone());
    // The page's clock, so "added now" reads right on a pinned demo too.
    let now = state.now();
    state
        .inner
        .db
        .call(move |c| tokens::create(c, &sub, &user, &name, &value, now))
        .await?;
    // Rendered directly (not redirected) so the secret is never in a URL.
    page(&state, &session, &jar, secret).await
}

#[derive(Deserialize)]
pub struct CsrfOnly {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn revoke_token(
    State(state): State<AppState>,
    User(session): User,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(form): Form<CsrfOnly>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let sub = session.sub.clone();
    let found = state
        .inner
        .db
        .call(move |c| tokens::revoke(c, &sub, id, Timestamp::now()))
        .await?;
    if !found {
        return Err(AppError::NotFound("no such device".into()));
    }
    Ok(Redirect::to("/settings").into_response())
}
