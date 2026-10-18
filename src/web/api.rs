//! `POST /api/ingest`: text from the iOS Shortcut. Bearer tokens only (no
//! cookies, so no CSRF), a small body, and a per-token rate limit.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::AppState;
use super::inbox::{ingest, text_message};
use crate::db::tokens;
use crate::model::Channel;

pub const MAX_BODY: usize = 256 * 1024;

/// Requests per token per window.
const RATE: usize = 30;
const WINDOW: Duration = Duration::from_secs(600);

/// A sliding-window limit per token id.
#[derive(Default)]
pub struct RateLimiter {
    hits: Mutex<HashMap<i64, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn allow(&self, token: i64, now: Instant) -> bool {
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        let list = hits.entry(token).or_default();
        list.retain(|t| now.duration_since(*t) < WINDOW);
        if list.len() >= RATE {
            return false;
        }
        list.push(now);
        true
    }
}

#[derive(Deserialize)]
pub struct IngestRequest {
    text: String,
    /// `share` (default) or `api`.
    #[serde(default)]
    channel: Option<String>,
}

#[derive(Serialize)]
pub struct IngestResponse {
    result: &'static str,
    /// One line for the Shortcut's notification.
    summary: String,
    message_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<PackageRef>,
}

#[derive(Serialize)]
pub struct PackageRef {
    id: i64,
    url: String,
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(serde_json::json!({ "error": message }))).into_response()
}

pub async fn ingest_api(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<IngestRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Some(secret) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
    else {
        return error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    let token = match state
        .inner
        .db
        .call(move |c| tokens::authenticate(c, &secret, Timestamp::now()))
        .await
    {
        Ok(Some(t)) => t,
        Ok(None) => return error(StatusCode::UNAUTHORIZED, "invalid or revoked token"),
        Err(e) => {
            tracing::error!(error = %e, "token lookup failed");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "database error");
        }
    };
    if !state.inner.rate_limiter.allow(token.id, Instant::now()) {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many messages, try again later",
        );
    }
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return error(StatusCode::BAD_REQUEST, &e.body_text()),
    };
    let channel = match req.channel.as_deref() {
        None | Some("share") => Channel::Share,
        Some("api") => Channel::Api,
        Some(_) => return error(StatusCode::BAD_REQUEST, "channel must be share or api"),
    };
    let incoming = match text_message(channel, &token.user_name, &req.text) {
        Ok(i) => i,
        Err(e) => return error(StatusCode::BAD_REQUEST, &e.to_string()),
    };
    match ingest(&state, incoming).await {
        Ok(p) => Json(IngestResponse {
            result: p.result.as_str(),
            summary: p.summary(),
            message_id: p.message_id,
            package: p.package_id.map(|id| PackageRef {
                id,
                url: format!(
                    "{}/packages/{id}",
                    state.inner.cfg.public_url.as_str().trim_end_matches('/')
                ),
            }),
        })
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "ingest failed");
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not store the message",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_is_per_token_and_slides() {
        let limiter = RateLimiter::default();
        let start = Instant::now();
        for _ in 0..RATE {
            assert!(limiter.allow(1, start));
        }
        assert!(!limiter.allow(1, start));
        assert!(limiter.allow(2, start), "other tokens are unaffected");
        assert!(limiter.allow(1, start + WINDOW));
    }
}
