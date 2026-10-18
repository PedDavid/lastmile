//! Drafting a rule with the model, from an inbox message: the admin reads
//! the text that would be sent, can edit it, and nothing is sent until they
//! click *Send*.

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
use crate::db::rules as rule_db;
use crate::ingest::MAX_TEXT;
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
    /// What is sent: the message, as far as the admin has edited it.
    text: String,
    hint: String,
    host: String,
    model: String,
    error: String,
}

#[derive(Deserialize)]
pub struct FormQuery {
    message: i64,
}

async fn load(state: &AppState, id: i64) -> Result<Message, AppError> {
    let message = state
        .inner
        .db
        .call(move |c| messages::get(c, id))
        .await?
        .ok_or_else(|| AppError::NotFound(format!("no message {id}")))?;
    if message.text().is_none() {
        return Err(AppError::BadRequest(
            "the text of this message was deleted after the retention period".into(),
        ));
    }
    Ok(message)
}

fn page(
    state: &AppState,
    session: &Session,
    jar: &CookieJar,
    message: &Message,
    text: Option<String>,
    hint: String,
    error: String,
) -> Result<DraftPage, AppError> {
    let client = state.inner.llm.as_ref().ok_or_else(llm_off)?;
    Ok(DraftPage {
        layout: Layout::new(state, session, jar, "Draft a rule"),
        message_id: message.id,
        channel: message.channel.label(),
        sender: message.sender.clone().unwrap_or_default(),
        text: text.unwrap_or_else(|| message.text().unwrap_or_default()),
        hint,
        host: client.host(),
        model: client.model().to_string(),
        error,
    })
}

pub async fn form(
    State(state): State<AppState>,
    Admin(session): Admin,
    jar: CookieJar,
    Query(q): Query<FormQuery>,
) -> Result<Response, AppError> {
    let message = load(&state, q.message).await?;
    let p = page(
        &state,
        &session,
        &jar,
        &message,
        None,
        String::new(),
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
    text: String,
    #[serde(default)]
    hint: String,
    /// `reset` (back to the message as it came in) or `send`.
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
    let message = load(&state, form.message_id).await?;
    let hint = form.hint.trim().to_string();
    if hint.len() > 500 {
        return Err(AppError::BadRequest("the description is too long".into()));
    }
    let show =
        |text: Option<String>, error: String, status: StatusCode| -> Result<Response, AppError> {
            let p = page(&state, &session, &jar, &message, text, hint.clone(), error)?;
            Ok((status, Html(p.render()?)).into_response())
        };
    // "Reset" throws the edits away and starts from the message again.
    if form.action != "send" {
        return show(None, String::new(), StatusCode::OK);
    }
    let text = form.text.replace("\r\n", "\n");
    if text.trim().is_empty() {
        return show(
            Some(text),
            "There is nothing to send.".into(),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
    }
    if text.len() > MAX_TEXT {
        return show(
            Some(text),
            format!("The text is longer than {} KiB.", MAX_TEXT / 1024),
            StatusCode::UNPROCESSABLE_ENTITY,
        );
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
            message: &text,
            sender: message.sender.as_deref(),
            channel: message.channel,
            hint: &hint,
            stored: &stored,
            samples: &samples,
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
        return show(Some(text), error, StatusCode::BAD_GATEWAY);
    }
    let Some(yaml) = drafted.yaml else {
        return show(
            Some(text),
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
