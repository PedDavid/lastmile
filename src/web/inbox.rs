//! The inbox: messages no rule turned into a package, the paste box, the
//! `.eml` dropbox and the share target.

use std::collections::HashMap;

use askama::Template;
use axum::Form;
use axum::extract::{Multipart, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use jiff::Timestamp;
use serde::Deserialize;

use super::auth::{Session, User};
use super::pages::Layout;
use super::views::relative;
use super::{AppError, AppState};
use crate::db::messages::{self, Message, MessageResult};
use crate::db::packages::{self, Scope};
use crate::db::rules as rule_db;
use crate::ingest::{self, Incoming, MAX_TEXT, Processed};
use crate::model::{Channel, Package};

/// Upload limits: files per drop, and bytes per file.
pub const MAX_FILES: usize = 10;
pub const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;

/// Stores and processes one message, counting it in the metrics.
pub async fn ingest(state: &AppState, incoming: Incoming) -> Result<Processed, AppError> {
    let channel = incoming.channel;
    let retention = state.inner.cfg.message_retention;
    let processed = state
        .inner
        .db
        .call(move |c| {
            let rules = ingest::load_rules(c)?;
            ingest::ingest(c, &rules, incoming, retention, Timestamp::now())
        })
        .await?;
    if processed.duplicate_of.is_none() {
        state.inner.metrics.message(channel, processed.result);
    }
    Ok(processed)
}

pub fn text_message(channel: Channel, by: &str, text: &str) -> Result<Incoming, AppError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AppError::BadRequest("the message is empty".into()));
    }
    if text.len() > MAX_TEXT {
        return Err(AppError::BadRequest(format!(
            "the message is longer than {} KiB",
            MAX_TEXT / 1024
        )));
    }
    Ok(Incoming {
        channel,
        submitted_by: Some(by.to_string()),
        sender: None,
        subject: None,
        sent_at: None,
        message_id: None,
        text: text.to_string(),
        raw: text.as_bytes().to_vec(),
    })
}

/// A message as the inbox shows it.
pub struct MessageRow {
    pub id: i64,
    pub channel: &'static str,
    pub received: String,
    pub from: String,
    pub subject: String,
    pub text: String,
    pub purged: bool,
    /// `unmatched`, `error` or `attach` (a rule matched a follow-up).
    pub kind: &'static str,
    pub rule: String,
    pub error: String,
    pub fields: Vec<(String, String)>,
}

impl MessageRow {
    fn new(m: &Message, rule_names: &HashMap<String, String>, now: Timestamp) -> MessageRow {
        let from = m
            .sender
            .clone()
            .or_else(|| m.submitted_by.as_ref().map(|u| format!("shared by {u}")))
            .unwrap_or_default();
        MessageRow {
            id: m.id,
            channel: m.channel.label(),
            received: relative(m.received_at, now),
            from,
            subject: m.subject.clone().unwrap_or_default(),
            text: m.body_text.clone().unwrap_or_default(),
            purged: m.body_text.is_none(),
            kind: match m.result {
                MessageResult::Error => "error",
                MessageResult::Matched => "attach",
                _ => "unmatched",
            },
            rule: m
                .rule_id
                .as_ref()
                .and_then(|id| rule_names.get(id).cloned())
                .unwrap_or_default(),
            error: m.error.clone().unwrap_or_default(),
            fields: m
                .extracted
                .iter()
                .map(|(k, v)| (k.replace('_', " "), v.clone()))
                .collect(),
        }
    }
}

/// One file of a drop, and what became of it.
pub struct UploadRow {
    pub name: String,
    pub ok: bool,
    pub summary: String,
    pub package_id: Option<i64>,
}

#[derive(Template)]
#[template(path = "inbox.html")]
struct InboxPage {
    layout: Layout,
    rows: Vec<MessageRow>,
    packages: Vec<(i64, String)>,
    uploads: Vec<UploadRow>,
    notice: String,
    max_files: usize,
    llm_enabled: bool,
}

#[derive(Deserialize, Default)]
pub struct InboxQuery {
    #[serde(default)]
    notice: String,
}

async fn inbox_page(
    state: &AppState,
    session: &Session,
    jar: &CookieJar,
    uploads: Vec<UploadRow>,
    notice: String,
) -> Result<Response, AppError> {
    let now = state.now();
    let (inbox, rules, active) = state
        .inner
        .db
        .call(|c| {
            Ok((
                messages::inbox(c)?,
                rule_db::list(c)?,
                packages::list(c, Scope::Active)?,
            ))
        })
        .await?;
    let names: HashMap<String, String> = rules.into_iter().map(|r| (r.id, r.def.name)).collect();
    let mut active: Vec<Package> = active;
    crate::model::sort_packages(&mut active);
    let packages = active
        .iter()
        .map(|p| {
            let label = match (&p.courier, &p.tracking_number) {
                (Some(c), Some(t)) => format!("{} · {c} {t}", p.title),
                _ => p.title.clone(),
            };
            (p.id, label)
        })
        .collect();
    Ok(Html(
        InboxPage {
            layout: Layout::new(state, session, jar, "Inbox"),
            rows: inbox
                .iter()
                .map(|m| MessageRow::new(m, &names, now))
                .collect(),
            packages,
            uploads,
            notice,
            max_files: MAX_FILES,
            llm_enabled: state.inner.llm.is_some(),
        }
        .render()?,
    )
    .into_response())
}

pub async fn inbox(
    State(state): State<AppState>,
    User(session): User,
    jar: CookieJar,
    Query(q): Query<InboxQuery>,
) -> Result<Response, AppError> {
    let notice = match q.notice.as_str() {
        "unmatched" => "No rule matched that message: it is waiting below.",
        "error" => "A rule matched but could not read everything: see below.",
        "attach" => "A rule matched, but the message doesn't say which package: attach it below.",
        "ignored" => "Message ignored.",
        "duplicate" => "That message was already received.",
        _ => "",
    };
    inbox_page(&state, &session, &jar, Vec::new(), notice.into()).await
}

#[derive(Deserialize)]
pub struct PasteForm {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
    text: String,
    /// `share` when sent from the share target, else `paste`.
    #[serde(default)]
    channel: String,
}

/// Where to send the person after a message went through the rules.
fn after(processed: &Processed) -> String {
    if processed.duplicate_of.is_some() {
        return "/inbox?notice=duplicate".into();
    }
    match (processed.result, processed.package_id) {
        (_, Some(id)) => format!(
            "/packages/{id}?notice={}",
            if processed.created {
                "from-message"
            } else {
                "updated-from-message"
            }
        ),
        (MessageResult::Error, None) => "/inbox?notice=error".into(),
        (MessageResult::Matched, None) => "/inbox?notice=attach".into(),
        _ => "/inbox?notice=unmatched".into(),
    }
}

pub async fn paste(
    State(state): State<AppState>,
    User(session): User,
    headers: HeaderMap,
    Form(form): Form<PasteForm>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let channel = if form.channel == "share" {
        Channel::Share
    } else {
        Channel::Paste
    };
    let processed = ingest(&state, text_message(channel, &session.name, &form.text)?).await?;
    Ok(Redirect::to(&after(&processed)).into_response())
}

/// The `.eml` dropbox: up to [`MAX_FILES`] files of [`MAX_FILE_BYTES`] each,
/// in a normal CSRF-protected multipart POST.
pub async fn upload(
    State(state): State<AppState>,
    User(session): User,
    jar: CookieJar,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let bad = |e: axum::extract::multipart::MultipartError| AppError::BadRequest(e.body_text());
    let mut csrf = None;
    let mut files: Vec<(String, Result<Vec<u8>, String>)> = Vec::new();
    while let Some(mut field) = multipart.next_field().await.map_err(bad)? {
        match field.name() {
            Some("_csrf") => csrf = Some(field.text().await.map_err(bad)?),
            Some("files") => {
                let name = field.file_name().unwrap_or("message.eml").to_string();
                if name.is_empty() {
                    continue; // the empty part of a form submitted without files
                }
                if files.len() == MAX_FILES {
                    return Err(AppError::BadRequest(format!(
                        "at most {MAX_FILES} files at a time"
                    )));
                }
                let mut bytes = Vec::new();
                let mut too_big = false;
                while let Some(chunk) = field.chunk().await.map_err(bad)? {
                    if bytes.len() + chunk.len() > MAX_FILE_BYTES {
                        too_big = true;
                        break;
                    }
                    bytes.extend_from_slice(&chunk);
                }
                let content = if too_big {
                    Err(format!("larger than {} MiB", MAX_FILE_BYTES / 1024 / 1024))
                } else {
                    Ok(bytes)
                };
                files.push((name, content));
            }
            _ => {}
        }
    }
    session.check_csrf(&headers, csrf.as_deref())?;
    if files.is_empty() {
        return Err(AppError::BadRequest("choose one or more .eml files".into()));
    }
    let mut rows = Vec::new();
    for (name, content) in files {
        let row = match content.and_then(|raw| {
            if name.to_lowercase().ends_with(".msg") {
                return Err("Outlook .msg files are not supported: save the email as .eml".into());
            }
            ingest::eml::parse(&raw).map(|email| (raw, email))
        }) {
            Err(e) => UploadRow {
                name,
                ok: false,
                summary: e,
                package_id: None,
            },
            Ok((raw, email)) => {
                let processed = ingest(
                    &state,
                    Incoming {
                        channel: Channel::Upload,
                        submitted_by: Some(session.name.clone()),
                        sender: email.sender,
                        subject: email.subject,
                        sent_at: email.date,
                        message_id: email.message_id,
                        text: email.text,
                        raw,
                    },
                )
                .await?;
                UploadRow {
                    name,
                    ok: processed.package_id.is_some(),
                    summary: processed.summary(),
                    package_id: processed.package_id,
                }
            }
        };
        rows.push(row);
    }
    inbox_page(&state, &session, &jar, rows, String::new()).await
}

#[derive(Deserialize)]
pub struct AttachForm {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
    package_id: i64,
}

pub async fn attach(
    State(state): State<AppState>,
    User(session): User,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(form): Form<AttachForm>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let by = session.name.clone();
    let package_id = form.package_id;
    let done = state
        .inner
        .db
        .call(move |c| ingest::attach(c, id, package_id, &by))
        .await?;
    if !done {
        return Err(AppError::NotFound("no such message or package".into()));
    }
    Ok(Redirect::to(&format!("/packages/{package_id}?notice=attached")).into_response())
}

#[derive(Deserialize)]
pub struct CsrfOnly {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn ignore(
    State(state): State<AppState>,
    User(session): User,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(form): Form<CsrfOnly>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    state
        .inner
        .db
        .call(move |c| messages::ignore(c, id))
        .await?;
    Ok(Redirect::to("/inbox?notice=ignored").into_response())
}

/// Joins what a share sheet sent. Apps often repeat themselves (the title
/// is the start of the text, the URL is in it): parts another one already
/// contains are dropped.
fn share_text(parts: &[&str]) -> String {
    let parts: Vec<&str> = parts
        .iter()
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect();
    parts
        .iter()
        .enumerate()
        .filter(|(i, p)| {
            !parts.iter().enumerate().any(|(j, other)| {
                j != *i && other.contains(**p) && (other.len() > p.len() || j < *i)
            })
        })
        .map(|(_, p)| *p)
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Deserialize, Default)]
pub struct ShareQuery {
    #[serde(default)]
    title: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    url: String,
}

#[derive(Template)]
#[template(path = "share.html")]
struct SharePage {
    layout: Layout,
    text: String,
}

/// The PWA share target (GET, so the `SameSite=Lax` session cookie comes
/// along). Nothing is stored until the person confirms with a normal POST.
pub async fn share(
    State(state): State<AppState>,
    User(session): User,
    jar: CookieJar,
    Query(q): Query<ShareQuery>,
) -> Result<Response, AppError> {
    let text = share_text(&[&q.title, &q.text, &q.url]);
    Ok(Html(
        SharePage {
            layout: Layout::new(&state, &session, &jar, "Shared message"),
            text,
        }
        .render()?,
    )
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_parts_are_not_repeated() {
        assert_eq!(
            share_text(&["CTT", "CTT: encomenda RR1PT", ""]),
            "CTT: encomenda RR1PT"
        );
        assert_eq!(share_text(&["x", "x", "x"]), "x");
        assert_eq!(
            share_text(&["Paack", "Entrega hoje", "https://track.paack.app/t/1"]),
            "Paack\nEntrega hoje\nhttps://track.paack.app/t/1"
        );
        assert_eq!(share_text(&["", " ", ""]), "");
    }
}
