//! Sharing rules: export one as text, a one-line blob or a file; import one
//! from a paste or a file, with a preview. Imported rules are saved disabled.

use askama::Template;
use axum::Form;
use axum::extract::{Multipart, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use jiff::Timestamp;
use serde::Deserialize;

use super::auth::Admin;
use super::pages::Layout;
use super::{AppError, AppState};
use crate::db::rules::{self as rule_db, Origin};
use crate::db::{messages, settings};
use crate::rules::engine::{self, Input, Outcome};
use crate::rules::share::{self, RuleDoc};

fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(200) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

#[derive(Template)]
#[template(path = "rule_export.html")]
struct ExportPage {
    layout: Layout,
    id: String,
    name: String,
    yaml: String,
    blob: String,
    samples: usize,
    dropped: usize,
    household_empty: bool,
    file_name: String,
}

async fn load_export(state: &AppState, id: String) -> Result<(RuleDoc, usize, bool), AppError> {
    let missing = format!("no rule {id}");
    let (rule, samples, household) = state
        .inner
        .db
        .call(move |c| {
            Ok((
                rule_db::get(c, &id)?,
                rule_db::all_samples(c)?,
                settings::household(c)?,
            ))
        })
        .await?;
    let rule = rule.ok_or(AppError::NotFound(missing))?;
    let (doc, dropped) = share::export(&rule, &samples, &household);
    Ok((doc, dropped, household.is_empty()))
}

/// What a rule looks like when shared, for the admin to read before
/// copying it anywhere.
pub async fn export_page(
    State(state): State<AppState>,
    Admin(session): Admin,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let (doc, dropped, household_empty) = load_export(&state, id).await?;
    Ok(Html(
        ExportPage {
            layout: Layout::new(&state, &session, &jar, "Share a rule"),
            id: doc.id.clone(),
            name: doc.def.name.clone(),
            yaml: doc.to_yaml(),
            blob: doc.to_blob(),
            samples: doc.samples.len(),
            dropped,
            household_empty,
            file_name: share::file_name(&doc.def),
        }
        .render()?,
    )
    .into_response())
}

pub async fn export_file(
    State(state): State<AppState>,
    Admin(_): Admin,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let (doc, _, _) = load_export(&state, id).await?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/yaml; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", share::file_name(&doc.def)),
            ),
        ],
        doc.to_yaml(),
    )
        .into_response())
}

#[derive(Template)]
#[template(path = "rule_import.html")]
struct ImportPage {
    layout: Layout,
    text: String,
    errors: Vec<String>,
}

pub async fn import_form(
    State(state): State<AppState>,
    Admin(session): Admin,
    jar: CookieJar,
) -> Result<Response, AppError> {
    Ok(Html(
        ImportPage {
            layout: Layout::new(&state, &session, &jar, "Import a rule"),
            text: String::new(),
            errors: Vec::new(),
        }
        .render()?,
    )
    .into_response())
}

pub struct SampleRow {
    pub text: String,
    pub ok: bool,
    pub error: String,
    pub expected: Vec<(String, String)>,
    pub got: Vec<(String, String)>,
}

pub struct DiffLine {
    /// `+`, `-` or ` `.
    pub sign: &'static str,
    pub text: String,
}

pub struct InboxMatch {
    pub text: String,
    pub fields: Vec<(String, String)>,
}

#[derive(Template)]
#[template(path = "rule_import_preview.html")]
struct PreviewPage {
    layout: Layout,
    source: String,
    name: String,
    slug: String,
    courier: String,
    priority: i64,
    /// `new`, or the stored rule's name when this updates it.
    updates: Option<String>,
    current_version: i64,
    incoming_version: i64,
    diff: Vec<DiffLine>,
    changed: bool,
    samples: Vec<SampleRow>,
    samples_pass: bool,
    inbox: Vec<InboxMatch>,
}

fn pairs(f: &engine::Fields) -> Vec<(String, String)> {
    f.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
}

/// Reads the pasted text or the uploaded file (the file wins), and shows
/// what importing it would do.
pub async fn import_preview(
    State(state): State<AppState>,
    Admin(session): Admin,
    jar: CookieJar,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let bad = |e: axum::extract::multipart::MultipartError| AppError::BadRequest(e.body_text());
    let (mut csrf, mut text, mut file) = (None, String::new(), Vec::new());
    while let Some(mut field) = multipart.next_field().await.map_err(bad)? {
        match field.name() {
            Some("_csrf") => csrf = Some(field.text().await.map_err(bad)?),
            Some("text") => text = field.text().await.map_err(bad)?,
            Some("file") => {
                while let Some(chunk) = field.chunk().await.map_err(bad)? {
                    if file.len() + chunk.len() > share::MAX_DOC * 2 {
                        return Err(AppError::BadRequest("the file is too large".into()));
                    }
                    file.extend_from_slice(&chunk);
                }
            }
            _ => {}
        }
    }
    session.check_csrf(&headers, csrf.as_deref())?;
    let source = if file.is_empty() {
        text
    } else {
        String::from_utf8(file).map_err(|_| AppError::BadRequest("the file is not text".into()))?
    };
    let layout = Layout::new(&state, &session, &jar, "Import a rule");
    match preview(&state, &source, layout).await? {
        Ok(page) => Ok(Html(page.render()?).into_response()),
        Err((layout, errors)) => {
            let page = ImportPage {
                layout,
                text: source,
                errors,
            };
            Ok((StatusCode::UNPROCESSABLE_ENTITY, Html(page.render()?)).into_response())
        }
    }
}

/// Parses and checks `source` and works out what importing it does; the
/// layout comes back with the errors, for the page that shows them.
async fn preview(
    state: &AppState,
    source: &str,
    layout: Layout,
) -> Result<Result<PreviewPage, (Layout, Vec<String>)>, AppError> {
    let doc = match share::parse(source) {
        Ok(d) => d,
        Err(e) => return Ok(Err((layout, vec![e]))),
    };
    let checked = match share::check(&doc) {
        Ok(c) => c,
        Err(errors) => return Ok(Err((layout, errors))),
    };
    let id = doc.id.clone();
    let (existing, inbox) = state
        .inner
        .db
        .call(move |c| Ok((rule_db::get(c, &id)?, messages::inbox(c)?)))
        .await?;
    let incoming = doc.def.to_yaml();
    let (updates, current_version, diff, changed) = match &existing {
        Some(r) => {
            let old = r.def.to_yaml();
            let diff = similar::TextDiff::from_lines(&old, &incoming)
                .iter_all_changes()
                .map(|c| DiffLine {
                    sign: match c.tag() {
                        similar::ChangeTag::Insert => "+",
                        similar::ChangeTag::Delete => "-",
                        similar::ChangeTag::Equal => " ",
                    },
                    text: c.value().trim_end_matches('\n').to_string(),
                })
                .collect();
            (Some(r.def.name.clone()), r.version, diff, old != incoming)
        }
        None => (None, 0, Vec::new(), true),
    };
    let matches = inbox
        .iter()
        .filter_map(|m| {
            let text = m.text()?;
            let outcome = engine::evaluate(
                std::slice::from_ref(&checked.compiled),
                &Input {
                    channel: m.channel,
                    sender: m.sender.as_deref(),
                    text: &text,
                    sent_at: m.sent_at,
                },
            );
            match outcome {
                Outcome::Matched { fields, .. } | Outcome::Failed { fields, .. } => {
                    Some(InboxMatch {
                        text: excerpt(&text),
                        fields: pairs(&fields),
                    })
                }
                Outcome::Unmatched => None,
            }
        })
        .collect();
    let samples_pass = checked.samples_pass();
    Ok(Ok(PreviewPage {
        layout,
        source: source.trim().to_string(),
        name: doc.def.name.clone(),
        slug: doc.def.slug.clone(),
        courier: doc.def.courier.clone().unwrap_or_default(),
        priority: doc.def.priority,
        updates,
        current_version,
        incoming_version: doc.version,
        diff,
        changed,
        samples: checked
            .samples
            .iter()
            .map(|s| SampleRow {
                text: excerpt(&s.text),
                ok: s.ok,
                error: s.error.clone().unwrap_or_default(),
                expected: pairs(&s.expected),
                got: pairs(&s.got),
            })
            .collect(),
        samples_pass,
        inbox: matches,
    }))
}

#[derive(Deserialize)]
pub struct ConfirmForm {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
    source: String,
}

/// Saves the previewed rule, disabled. It is checked again: the form
/// carries the text, which is never trusted just because it was shown.
pub async fn import_confirm(
    State(state): State<AppState>,
    Admin(session): Admin,
    headers: HeaderMap,
    Form(form): Form<ConfirmForm>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let doc = share::parse(&form.source).map_err(AppError::BadRequest)?;
    let checked = share::check(&doc).map_err(|e| AppError::BadRequest(e.join("; ")))?;
    if !checked.samples_pass() {
        return Err(AppError::BadRequest(
            "a sample of the rule does not give the fields it claims".into(),
        ));
    }
    let by = session.name.clone();
    let id = doc.id.clone();
    state
        .inner
        .db
        .call(move |c| {
            let tx = c.transaction()?;
            let existing = rule_db::get(&tx, &doc.id)?;
            let now = Timestamp::now();
            rule_db::save(
                &tx,
                &doc.id,
                &doc.def.to_yaml(),
                &doc.def,
                Origin::Imported,
                false,
                &by,
                now,
            )?;
            // Disabled until an admin looks at it, even if it replaces a
            // rule that was enabled.
            rule_db::set_enabled(&tx, &doc.id, false)?;
            let have: Vec<String> = rule_db::all_samples(&tx)?
                .into_iter()
                .filter(|s| s.rule_id == doc.id)
                .map(|s| s.text)
                .collect();
            for s in &doc.samples {
                if existing.is_some() && have.contains(&s.text) {
                    continue;
                }
                rule_db::add_sample(
                    &tx,
                    &doc.id,
                    None,
                    s.channel,
                    s.sender.as_deref(),
                    &s.text,
                    &s.expected,
                    now,
                )?;
            }
            tx.commit()
        })
        .await?;
    Ok(Redirect::to(&format!("/rules/{id}?notice=imported")).into_response())
}
