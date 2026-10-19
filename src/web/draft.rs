//! Drafting a rule with the model, from an inbox message: the admin sees
//! the message redacted next to the original, can edit it, and nothing is
//! sent until they click *Send*.

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
use crate::redact::{Found, Household, Options, find_leak, redact_known, redact_with};
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
    counts: Vec<(String, usize)>,
    hint: String,
    redact_codes: bool,
    host: String,
    model: String,
    household_empty: bool,
    /// Where the analyzer is, when it is on and answered.
    analyzer: String,
    /// Why the analyzer's suggestions are missing, when it is on and didn't
    /// answer.
    analyzer_warning: String,
    error: String,
}

/// What the analyzer service found in the original message.
#[derive(Default)]
struct Analysis {
    found: Vec<Found>,
    host: String,
    warning: String,
}

/// Asks the analyzer, if there is one. A failure only costs the
/// suggestions: the household values and patterns still apply, and the admin
/// is told.
async fn analyze(state: &AppState, original: &str) -> Analysis {
    let Some(client) = &state.inner.analyzer else {
        return Analysis::default();
    };
    match client.analyze(original).await {
        Ok(found) => {
            state.inner.metrics.analyzer_call(true);
            Analysis {
                found,
                host: client.host(),
                warning: String::new(),
            }
        }
        Err(e) => {
            state.inner.metrics.analyzer_call(false);
            tracing::warn!(error = %e, "the analyzer did not answer");
            Analysis {
                warning: format!(
                    "The analyzer at {} did not answer ({e}), so names and places that are not in the household values are not replaced. Read the text on the right with care.",
                    client.host()
                ),
                ..Analysis::default()
            }
        }
    }
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
    redact_codes: bool,
    analysis: Analysis,
    error: String,
) -> Result<DraftPage, AppError> {
    let client = state.inner.llm.as_ref().ok_or_else(llm_off)?;
    let original = message.text().unwrap_or_default();
    // What the automatic redaction found, whatever has been edited since.
    let auto = redact_with(
        &original,
        household,
        Options { redact_codes },
        &analysis.found,
    );
    let counts = counts(&auto);
    let redacted = redacted.unwrap_or(auto.text);
    Ok(DraftPage {
        layout: Layout::new(state, session, jar, "Draft a rule"),
        message_id: message.id,
        channel: message.channel.label(),
        sender: message
            .sender
            .as_deref()
            .map(|s| redact_known(s, household))
            .unwrap_or_default(),
        original,
        redacted,
        counts,
        hint,
        redact_codes,
        host: client.host(),
        model: client.model().to_string(),
        household_empty: household.is_empty(),
        analyzer: analysis.host,
        analyzer_warning: analysis.warning,
        error,
    })
}

fn counts(r: &crate::redact::Redacted) -> Vec<(String, usize)> {
    r.counts.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

pub async fn form(
    State(state): State<AppState>,
    Admin(session): Admin,
    jar: CookieJar,
    Query(q): Query<FormQuery>,
) -> Result<Response, AppError> {
    let (message, household) = load(&state, q.message).await?;
    let analysis = analyze(&state, &message.text().unwrap_or_default()).await;
    let p = page(
        &state,
        &session,
        &jar,
        &message,
        &household,
        None,
        String::new(),
        false,
        analysis,
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
    redact_codes: Option<String>,
    /// `redact` (again, from the original) or `send`.
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
    let redact_codes = form.redact_codes.is_some();
    let hint = form.hint.trim().to_string();
    if hint.len() > 500 {
        return Err(AppError::BadRequest("the description is too long".into()));
    }
    let text = message.text().unwrap_or_default();
    let analysis = || analyze(&state, &text);
    let show = |redacted: Option<String>,
                error: String,
                status: StatusCode,
                analysis: Analysis|
     -> Result<Response, AppError> {
        let p = page(
            &state,
            &session,
            &jar,
            &message,
            &household,
            redacted,
            hint.clone(),
            redact_codes,
            analysis,
            error,
        )?;
        Ok((status, Html(p.render()?)).into_response())
    };
    // "Redact again" throws the edits away and starts from the original.
    if form.action != "send" {
        return show(None, String::new(), StatusCode::OK, analysis().await);
    }
    let redacted = form.redacted.replace("\r\n", "\n");
    if redacted.trim().is_empty() {
        return show(
            Some(redacted),
            "There is nothing to send.".into(),
            StatusCode::UNPROCESSABLE_ENTITY,
            analysis().await,
        );
    }
    if redacted.len() > MAX_TEXT {
        return show(
            Some(redacted),
            format!("The text is longer than {} KiB.", MAX_TEXT / 1024),
            StatusCode::UNPROCESSABLE_ENTITY,
            analysis().await,
        );
    }
    // Refused here, before anything is built to send: a household value is
    // still in the text (typed back in, or one the patterns missed).
    for text in [redacted.as_str(), hint.as_str()] {
        if let Some(kind) = find_leak(text, &household) {
            return show(
                Some(redacted.clone()),
                format!(
                    "Not sent: the text still contains a household {}. Replace it (or use *Redact again*).",
                    kind.label().to_lowercase()
                ),
                StatusCode::UNPROCESSABLE_ENTITY,
                analysis().await,
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
        return show(
            Some(redacted),
            error,
            StatusCode::BAD_GATEWAY,
            analysis().await,
        );
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
            analysis().await,
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
