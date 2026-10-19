//! AI assist: review redactions in place, with a per-user default.
//! Redact and Back to original never send; only an explicit submit calls the model.

use askama::Template;
use axum::Form;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::CookieJar;
use jiff::Timestamp;
use serde::Deserialize;

use super::auth::{Admin, Session};
use super::pages::Layout;
use super::rules::{EditorForm, editor_page};
use super::{AppError, AppState};
use crate::db::messages::{self, Message};
use crate::db::{rules as rule_db, settings};
use crate::ingest::MAX_TEXT;
use crate::redact::{Household, Options, RedactionPart, find_leak, redact, redact_known};
use crate::rules::draft::{self, Request};

fn llm_off() -> AppError {
    AppError::NotFound("the model that drafts rules is not configured (--llm-model)".into())
}

#[derive(Template)]
#[template(path = "rule_draft.html")]
struct DraftPage {
    layout: Layout,
    message_id: i64,
    channel: &'static str,
    sender: String,
    original: String,
    redacted: String,
    parts: Vec<RedactionPart>,
    parts_json: String,
    hint: String,
    is_redacted: bool,
    host: String,
    model: String,
    household_empty: bool,
    error: String,
}

#[derive(Deserialize)]
pub struct FormQuery {
    message: i64,
}

async fn load(state: &AppState, id: i64) -> Result<(Message, Household), AppError> {
    let (message, household) = state
        .inner
        .db
        .call(move |c| Ok((messages::get(c, id)?, settings::household(c)?)))
        .await?;
    let message = message.ok_or_else(|| AppError::NotFound(format!("no message {id}")))?;
    if message.text().is_none() {
        return Err(AppError::BadRequest(
            "the text of this message was deleted after the retention period".into(),
        ));
    }
    Ok((message, household))
}

#[allow(clippy::too_many_arguments)]
fn page(
    state: &AppState,
    session: &Session,
    jar: &CookieJar,
    message: &Message,
    household: &Household,
    redacted: Option<String>,
    hint: String,
    is_redacted: bool,
    error: String,
) -> Result<DraftPage, AppError> {
    let client = state.inner.llm.as_ref().ok_or_else(llm_off)?;
    let original = message.text().unwrap_or_default();
    // What the automatic redaction found, whatever has been edited since.
    let auto = redact(&original, household, Options { redact_codes: true });
    let redacted = redacted.unwrap_or_else(|| {
        if is_redacted {
            auto.text.clone()
        } else {
            original.clone()
        }
    });
    let parts = if redacted == auto.text {
        auto.parts
    } else {
        vec![RedactionPart {
            original: String::new(),
            text: redacted.clone(),
            kind: String::new(),
        }]
    };
    let parts_json = serde_json::to_string(&parts).expect("review parts serialize");
    Ok(DraftPage {
        layout: Layout::new(state, session, jar, "AI assist"),
        message_id: message.id,
        channel: message.channel.label(),
        sender: message
            .sender
            .as_deref()
            .map(|s| redact_known(s, household))
            .unwrap_or_default(),
        original,
        redacted,
        parts,
        parts_json,
        hint,
        is_redacted,
        host: client.host(),
        model: client.model().to_string(),
        household_empty: household.is_empty(),
        error,
    })
}

pub async fn form(
    State(state): State<AppState>,
    Admin(session): Admin,
    jar: CookieJar,
    Query(q): Query<FormQuery>,
) -> Result<Response, AppError> {
    let (message, household) = load(&state, q.message).await?;
    let sub = session.sub.clone();
    let is_redacted = state
        .inner
        .db
        .call(move |c| settings::redact_by_default(c, &sub))
        .await?;
    let p = page(
        &state,
        &session,
        &jar,
        &message,
        &household,
        None,
        String::new(),
        is_redacted,
        String::new(),
    )?;
    Ok(Html(p.render()?).into_response())
}

#[derive(Deserialize)]
pub struct DraftForm {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
    message_id: i64,
    #[serde(default)]
    redacted: String,
    #[serde(default)]
    hint: String,
    #[serde(default)]
    is_redacted: Option<String>,
    #[serde(default)]
    parts: String,
    /// `redact`, `original`, or explicit `send`.
    #[serde(default)]
    action: String,
}

pub async fn draft(
    State(state): State<AppState>,
    Admin(session): Admin,
    jar: CookieJar,
    headers: HeaderMap,
    Form(form): Form<DraftForm>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let client = state.inner.llm.as_ref().ok_or_else(llm_off)?;
    let (message, household) = load(&state, form.message_id).await?;
    let is_redacted = match form.action.as_str() {
        "redact" => true,
        "original" => false,
        _ => form.is_redacted.as_deref() == Some("1"),
    };
    let hint = form.hint.trim().to_string();
    if hint.len() > 500 {
        return Err(AppError::BadRequest("the description is too long".into()));
    }
    let show = |redacted: Option<String>,
                error: String,
                status: StatusCode|
     -> Result<Response, AppError> {
        let mut p = page(
            &state,
            &session,
            &jar,
            &message,
            &household,
            redacted,
            hint.clone(),
            is_redacted,
            error,
        )?;
        // Preserve chip metadata after validation/model errors without ever forwarding it.
        if form.action == "send"
            && form.parts.len() <= MAX_TEXT * 8
            && let Ok(parts) = serde_json::from_str::<Vec<RedactionPart>>(&form.parts)
        {
            let text: String = parts.iter().map(|p| p.text.as_str()).collect();
            if text == p.redacted {
                p.parts_json = serde_json::to_string(&parts).expect("review parts serialize");
                p.parts = parts;
            }
        }
        Ok((status, Html(p.render()?)).into_response())
    };
    // Redact and Back to original are local review actions; neither calls the model.
    if form.action != "send" {
        return show(None, String::new(), StatusCode::OK);
    }
    let redacted = form.redacted.replace("\r\n", "\n");
    if redacted.trim().is_empty() {
        return show(
            Some(redacted),
            "There is nothing to send.".into(),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    }
    if redacted.len() > MAX_TEXT {
        return show(
            Some(redacted),
            format!("The text is longer than {} KiB.", MAX_TEXT / 1024),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    }
    // Refused here, before anything is built to send: a household value is
    // still in the text (typed back in, or one the patterns missed).
    for text in [redacted.as_str(), hint.as_str()] {
        if let Some(kind) = find_leak(text, &household) {
            return show(
                Some(redacted.clone()),
                format!(
                    "Not sent: the text still contains a household {}. Hide it before submitting (or use Redact message).",
                    kind.label().to_lowercase()
                ),
                StatusCode::UNPROCESSABLE_ENTITY,
            );
        }
    }

    let (stored, samples) = state
        .inner
        .db
        .call(|c| Ok((rule_db::list(c)?, rule_db::all_samples(c)?)))
        .await?;
    let original = message.text().unwrap_or_default();
    let drafted = draft::draft(
        client,
        &Request {
            original: &original,
            original_sender: message.sender.as_deref(),
            redacted: &redacted,
            channel: message.channel,
            hint: &hint,
            stored: &stored,
            samples: &samples,
            household: &household,
            sent_at: message.sent_at.min(Timestamp::now()),
        },
    )
    .await;
    for call in &drafted.calls {
        state
            .inner
            .metrics
            .llm_call(call.ok, call.input_tokens, call.output_tokens);
    }
    if let Some(error) = drafted.error {
        return show(Some(redacted), error, StatusCode::BAD_GATEWAY);
    }
    let Some(yaml) = drafted.yaml else {
        return show(
            Some(redacted),
            format!(
                "The model did not return a rule after {} tries: {}",
                drafted.attempts,
                drafted.problems.join(" ")
            ),
            StatusCode::BAD_GATEWAY,
        );
    };
    // Into the editor, to be reviewed, edited and saved. The original is the
    // test message, and becomes the rule's first sample when saved.
    let ok = drafted.problems.is_empty();
    let form = EditorForm {
        source: yaml,
        test_text: original,
        test_channel: message.channel.as_str().to_string(),
        test_sender: message.sender.clone().unwrap_or_default(),
        message_id: Some(message.id),
        add_sample: ok.then(|| "1".to_string()),
        explanation: drafted.explanation,
        drafted_by: Some(format!(
            "{} ({} {})",
            client.model(),
            drafted.attempts,
            if drafted.attempts == 1 {
                "request"
            } else {
                "requests"
            }
        )),
        problems: drafted.problems,
        ..Default::default()
    };
    editor_page(
        &state,
        &session,
        &jar,
        None,
        form,
        String::new(),
        false,
        StatusCode::OK,
    )
    .await
}
