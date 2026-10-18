//! Rules (admins): the list, and the editor with its live preview. Saving
//! re-runs every rule's samples and refuses a change that breaks one unless
//! confirmed, then re-runs the inbox.

use std::collections::HashMap;

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use jiff::Timestamp;
use rusqlite::Connection;
use serde::Deserialize;

use super::auth::{Admin, Session};
use super::pages::Layout;
use super::views::relative;
use super::{AppError, AppState};
use crate::db::messages;
use crate::db::rules::{self as rule_db, Origin, Sample, StoredRule};
use crate::ingest;
use crate::model::Channel;
use crate::rules::engine::{self, CompiledRule, Fields, Input, Outcome};
use crate::rules::schema::{NEW_RULE, RuleDef};
use crate::rules::{SampleCheck, check_samples, with_candidate};

pub struct RuleRow {
    pub id: String,
    pub name: String,
    pub courier: String,
    pub priority: i64,
    pub enabled: bool,
    pub imported: bool,
    pub version: i64,
    pub hits: i64,
    pub last_match: String,
    pub samples: usize,
}

#[derive(Template)]
#[template(path = "rules.html")]
struct RulesPage {
    layout: Layout,
    rows: Vec<RuleRow>,
    inbox: usize,
}

pub async fn list(
    State(state): State<AppState>,
    Admin(session): Admin,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let now = state.now();
    let (rules, hits, samples, inbox) = state
        .inner
        .db
        .call(|c| {
            Ok((
                rule_db::list(c)?,
                messages::rule_hits(c)?,
                rule_db::all_samples(c)?,
                messages::inbox(c)?.len(),
            ))
        })
        .await?;
    let hits: HashMap<String, (i64, Timestamp)> =
        hits.into_iter().map(|(id, n, at)| (id, (n, at))).collect();
    let rows = rules
        .iter()
        .map(|r| {
            let (n, last) = hits.get(&r.id).copied().unzip();
            RuleRow {
                id: r.id.clone(),
                name: r.def.name.clone(),
                courier: r.def.courier.clone().unwrap_or_default(),
                priority: r.def.priority,
                enabled: r.enabled,
                imported: r.origin == Origin::Imported,
                version: r.version,
                hits: n.unwrap_or(0),
                last_match: last
                    .map(|t| relative(t, now))
                    .unwrap_or_else(|| "never".into()),
                samples: samples.iter().filter(|s| s.rule_id == r.id).count(),
            }
        })
        .collect();
    Ok(Html(
        RulesPage {
            layout: Layout::new(&state, &session, &jar, "Rules"),
            rows,
            inbox,
        }
        .render()?,
    )
    .into_response())
}

/// The editor's fields, as typed.
#[derive(Deserialize, Default, Clone)]
pub struct EditorForm {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
    #[serde(default)]
    source: String,
    /// A message to try the rule on (prefilled from the inbox).
    #[serde(default)]
    test_text: String,
    #[serde(default)]
    test_channel: String,
    #[serde(default)]
    test_sender: String,
    #[serde(default)]
    message_id: Option<i64>,
    /// Keep the test message as a sample of the rule.
    #[serde(default)]
    add_sample: Option<String>,
    /// Save even though samples change.
    #[serde(default)]
    confirm: Option<String>,
}

impl EditorForm {
    fn channel(&self) -> Channel {
        Channel::parse(&self.test_channel).unwrap_or(Channel::Share)
    }

    fn sender(&self) -> Option<&str> {
        Some(self.test_sender.trim()).filter(|s| !s.is_empty())
    }
}

pub struct FieldRow {
    pub name: String,
    pub value: String,
}

fn rows(fields: &Fields) -> Vec<FieldRow> {
    fields
        .iter()
        .map(|(k, v)| FieldRow {
            name: k.clone(),
            value: v.clone(),
        })
        .collect()
}

/// The test message, run through the rule being edited and through all
/// the rules (another one might win).
pub struct TestResult {
    pub matches: bool,
    pub fields: Vec<FieldRow>,
    pub error: String,
    /// The rule that would actually take the message, if not this one.
    pub winner: String,
}

pub struct SampleRow {
    pub id: i64,
    pub own: bool,
    pub rule: String,
    pub text: String,
    pub passes: bool,
    pub conflict: bool,
    pub got_rule: String,
    pub got: Vec<FieldRow>,
    pub expected: Vec<FieldRow>,
    pub error: String,
}

pub struct InboxMatch {
    pub id: i64,
    pub text: String,
    pub fields: Vec<FieldRow>,
    pub error: String,
}

#[derive(Template)]
#[template(path = "partials/rule_preview.html")]
pub struct Preview {
    /// Empty for a rule not saved yet.
    pub rule_id: String,
    pub csrf: String,
    pub errors: Vec<String>,
    pub test: Option<TestResult>,
    pub samples: Vec<SampleRow>,
    pub inbox: Vec<InboxMatch>,
}

impl Preview {
    fn conflicts(&self) -> usize {
        self.samples.iter().filter(|s| s.conflict).count()
    }
}

fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(160) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    }
}

/// Everything the editor shows about a candidate rule.
struct Evaluation {
    preview: Preview,
    compiled: Option<CompiledRule>,
    checks: Vec<SampleCheck>,
    samples: Vec<Sample>,
}

fn evaluate(
    conn: &Connection,
    id: &str,
    form: &EditorForm,
    now: Timestamp,
) -> rusqlite::Result<Evaluation> {
    let stored = rule_db::list(conn)?;
    let samples = rule_db::all_samples(conn)?;
    let names: HashMap<&str, &str> = stored
        .iter()
        .map(|r| (r.id.as_str(), r.def.name.as_str()))
        .collect();
    let compiled = RuleDef::from_yaml(&form.source)
        .map_err(|e| vec![e])
        .and_then(|def| engine::compile(id, 0, def));
    let compiled = match compiled {
        Ok(c) => c,
        Err(errors) => {
            return Ok(Evaluation {
                preview: Preview {
                    rule_id: id.to_string(),
                    csrf: String::new(),
                    errors,
                    test: None,
                    samples: Vec::new(),
                    inbox: Vec::new(),
                },
                compiled: None,
                checks: Vec::new(),
                samples,
            });
        }
    };
    let (before, after) = with_candidate(&stored, clone_compiled(&compiled));
    let checks = check_samples(&before, &after, id, &samples);
    let sample_rows = checks
        .iter()
        .zip(&samples)
        .filter(|(c, _)| c.own || c.passes_before != c.passes_after)
        .map(|(c, s)| SampleRow {
            id: s.id,
            own: c.own,
            rule: names.get(s.rule_id.as_str()).unwrap_or(&"?").to_string(),
            text: excerpt(&s.text),
            passes: c.passes_after,
            conflict: c.is_conflict(),
            got_rule: c.got_rule.clone().unwrap_or_default(),
            got: rows(&c.got),
            expected: rows(&s.expected),
            error: c.got_error.clone().unwrap_or_default(),
        })
        .collect();

    let test = (!form.test_text.trim().is_empty()).then(|| {
        let input = Input {
            channel: form.channel(),
            sender: form.sender(),
            text: &form.test_text,
            sent_at: now,
        };
        let alone = engine::evaluate(std::slice::from_ref(&compiled), &input);
        let all = engine::evaluate(&after, &input);
        let winner = all
            .rule()
            .map(|i| &after[i])
            .filter(|r| r.id != id)
            .map(|r| r.def.name.clone())
            .unwrap_or_default();
        let (matches, fields, error) = match alone {
            Outcome::Matched { fields, .. } => (true, fields, String::new()),
            Outcome::Failed { fields, error, .. } => (true, fields, error),
            Outcome::Unmatched => (false, Fields::new(), String::new()),
        };
        TestResult {
            matches,
            fields: rows(&fields),
            error,
            winner,
        }
    });

    let inbox = messages::inbox(conn)?
        .into_iter()
        .filter_map(|m| {
            let text = m.text()?;
            let input = Input {
                channel: m.channel,
                sender: m.sender.as_deref(),
                text: &text,
                sent_at: m.sent_at,
            };
            match engine::evaluate(std::slice::from_ref(&compiled), &input) {
                Outcome::Unmatched => None,
                Outcome::Matched { fields, .. } => Some(InboxMatch {
                    id: m.id,
                    text: excerpt(&text),
                    fields: rows(&fields),
                    error: String::new(),
                }),
                Outcome::Failed { fields, error, .. } => Some(InboxMatch {
                    id: m.id,
                    text: excerpt(&text),
                    fields: rows(&fields),
                    error,
                }),
            }
        })
        .collect();

    Ok(Evaluation {
        preview: Preview {
            rule_id: id.to_string(),
            csrf: String::new(),
            errors: Vec::new(),
            test,
            samples: sample_rows,
            inbox,
        },
        compiled: Some(compiled),
        checks,
        samples,
    })
}

fn clone_compiled(c: &CompiledRule) -> CompiledRule {
    engine::compile(&c.id, c.version, c.def.clone()).expect("compiled once already")
}

#[derive(Template)]
#[template(path = "rule_editor.html")]
struct EditorPage {
    layout: Layout,
    /// `None` for a new rule.
    rule: Option<StoredRule>,
    form: EditorForm,
    preview: Preview,
    channels: Vec<(&'static str, &'static str)>,
    notice: String,
    /// Saving was refused: samples change and need confirming.
    needs_confirm: bool,
}

impl EditorPage {
    fn action(&self) -> String {
        match &self.rule {
            Some(r) => format!("/rules/{}", r.id),
            None => "/rules".into(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn editor_page(
    state: &AppState,
    session: &Session,
    jar: &CookieJar,
    rule: Option<StoredRule>,
    form: EditorForm,
    notice: String,
    needs_confirm: bool,
    status: StatusCode,
) -> Result<Response, AppError> {
    let id = rule.as_ref().map(|r| r.id.clone()).unwrap_or_default();
    let form_for_eval = form.clone();
    let now = state.now();
    let mut eval = state
        .inner
        .db
        .call(move |c| evaluate(c, &id, &form_for_eval, now))
        .await?;
    eval.preview.csrf = session.csrf.clone();
    let title = rule
        .as_ref()
        .map(|r| r.def.name.clone())
        .unwrap_or_else(|| "New rule".into());
    let page = EditorPage {
        layout: Layout::new(state, session, jar, title),
        rule,
        form,
        preview: eval.preview,
        channels: Channel::ALL
            .iter()
            .map(|c| (c.as_str(), c.label()))
            .collect(),
        notice,
        needs_confirm,
    };
    Ok((status, Html(page.render()?)).into_response())
}

#[derive(Deserialize, Default)]
pub struct NewQuery {
    /// Start from an inbox message.
    message: Option<i64>,
}

pub async fn new(
    State(state): State<AppState>,
    Admin(session): Admin,
    jar: CookieJar,
    Query(q): Query<NewQuery>,
) -> Result<Response, AppError> {
    let mut form = EditorForm {
        source: NEW_RULE.into(),
        test_channel: Channel::Share.as_str().into(),
        ..Default::default()
    };
    if let Some(id) = q.message {
        let m = state
            .inner
            .db
            .call(move |c| messages::get(c, id))
            .await?
            .ok_or_else(|| AppError::NotFound(format!("no message {id}")))?;
        form.test_text = m.text().unwrap_or_default();
        form.test_channel = m.channel.as_str().into();
        form.test_sender = m.sender.unwrap_or_default();
        form.message_id = Some(id);
        form.add_sample = Some("1".into());
    }
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

#[derive(Deserialize, Default)]
pub struct EditQuery {
    #[serde(default)]
    notice: String,
    #[serde(default)]
    resolved: usize,
}

async fn get_rule(state: &AppState, id: String) -> Result<StoredRule, AppError> {
    let missing = format!("no rule {id}");
    state
        .inner
        .db
        .call(move |c| rule_db::get(c, &id))
        .await?
        .ok_or(AppError::NotFound(missing))
}

pub async fn edit(
    State(state): State<AppState>,
    Admin(session): Admin,
    jar: CookieJar,
    Path(id): Path<String>,
    Query(q): Query<EditQuery>,
) -> Result<Response, AppError> {
    let rule = get_rule(&state, id).await?;
    let form = EditorForm {
        source: rule.source.clone(),
        test_channel: Channel::Share.as_str().into(),
        ..Default::default()
    };
    let notice = match (q.notice.as_str(), q.resolved) {
        ("saved", 0) => "Saved.".to_string(),
        ("saved", 1) => "Saved. 1 inbox message is now a package.".to_string(),
        ("saved", n) => format!("Saved. {n} inbox messages are now packages."),
        ("enabled", n) if n > 0 => format!("Enabled. {n} inbox messages are now packages."),
        ("enabled", _) => "Enabled.".to_string(),
        ("disabled", _) => "Disabled: it no longer reads new messages.".to_string(),
        ("sample-deleted", _) => "Sample deleted.".to_string(),
        _ => String::new(),
    };
    editor_page(
        &state,
        &session,
        &jar,
        Some(rule),
        form,
        notice,
        false,
        StatusCode::OK,
    )
    .await
}

/// The live preview (htmx): compile errors, the test message, samples that
/// change, and the inbox messages the rule would now take.
pub async fn preview_new(
    state: State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    form: Form<EditorForm>,
) -> Result<Response, AppError> {
    preview(state, admin, headers, String::new(), form).await
}

pub async fn preview_existing(
    state: State<AppState>,
    admin: Admin,
    headers: HeaderMap,
    Path(id): Path<String>,
    form: Form<EditorForm>,
) -> Result<Response, AppError> {
    preview(state, admin, headers, id, form).await
}

async fn preview(
    State(state): State<AppState>,
    Admin(session): Admin,
    headers: HeaderMap,
    id: String,
    Form(form): Form<EditorForm>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let now = state.now();
    let mut eval = state
        .inner
        .db
        .call(move |c| evaluate(c, &id, &form, now))
        .await?;
    eval.preview.csrf = session.csrf.clone();
    Ok(Html(eval.preview.render()?).into_response())
}

pub async fn create(
    state: State<AppState>,
    admin: Admin,
    jar: CookieJar,
    headers: HeaderMap,
    form: Form<EditorForm>,
) -> Result<Response, AppError> {
    save(state, admin, jar, headers, None, form).await
}

pub async fn update(
    state: State<AppState>,
    admin: Admin,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<String>,
    form: Form<EditorForm>,
) -> Result<Response, AppError> {
    save(state, admin, jar, headers, Some(id), form).await
}

enum Saved {
    Done { id: String, resolved: usize },
    Invalid(String),
    NeedsConfirm,
}

async fn save(
    State(state): State<AppState>,
    Admin(session): Admin,
    jar: CookieJar,
    headers: HeaderMap,
    id: Option<String>,
    Form(form): Form<EditorForm>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let existing = match id {
        Some(id) => Some(get_rule(&state, id).await?),
        None => None,
    };
    let rule_id = existing
        .as_ref()
        .map(|r| r.id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let by = session.name.clone();
    let submitted = form.clone();
    let enabled = existing.as_ref().is_none_or(|r| r.enabled);
    let saved = state
        .inner
        .db
        .call(move |c| {
            let now = Timestamp::now();
            let eval = evaluate(c, &rule_id, &submitted, now)?;
            let Some(compiled) = eval.compiled else {
                return Ok(Saved::Invalid(eval.preview.errors.join("\n")));
            };
            let mut sample = None;
            if submitted.add_sample.is_some() && !submitted.test_text.trim().is_empty() {
                match eval.preview.test.as_ref() {
                    Some(t) if t.matches && t.error.is_empty() => {
                        let fields: Fields =
                            t.fields.iter().map(|f| (f.name.clone(), f.value.clone())).collect();
                        sample = Some(fields);
                    }
                    _ => {
                        return Ok(Saved::Invalid(
                            "The test message must match the rule, with every field read, to be kept as a sample."
                                .into(),
                        ));
                    }
                }
            }
            if eval.preview.conflicts() > 0 && submitted.confirm.is_none() {
                return Ok(Saved::NeedsConfirm);
            }
            let tx = c.transaction()?;
            rule_db::save(
                &tx,
                &rule_id,
                &submitted.source,
                &compiled.def,
                Origin::Local,
                enabled,
                &by,
                now,
            )?;
            // Confirmed: the rule's own samples now expect what it reads.
            for (check, s) in eval.checks.iter().zip(&eval.samples) {
                if check.own && !check.passes_after && check.got_error.is_none() {
                    rule_db::set_expected(&tx, s.id, &check.got)?;
                }
            }
            if let Some(expected) = sample {
                rule_db::add_sample(
                    &tx,
                    &rule_id,
                    submitted.message_id,
                    submitted.channel(),
                    submitted.sender(),
                    submitted.test_text.trim(),
                    &expected,
                    now,
                )?;
            }
            tx.commit()?;
            let rules = ingest::load_rules(c)?;
            let resolved = ingest::rerun_inbox(c, &rules, now)?;
            Ok(Saved::Done {
                id: rule_id,
                resolved,
            })
        })
        .await?;
    match saved {
        Saved::Done { id, resolved } => Ok(Redirect::to(&format!(
            "/rules/{id}?notice=saved&resolved={resolved}"
        ))
        .into_response()),
        Saved::Invalid(error) => {
            editor_page(
                &state,
                &session,
                &jar,
                existing,
                form,
                error,
                false,
                StatusCode::UNPROCESSABLE_ENTITY,
            )
            .await
        }
        Saved::NeedsConfirm => {
            editor_page(
                &state,
                &session,
                &jar,
                existing,
                form,
                "Saving changes what other samples get. Check them below and confirm.".into(),
                true,
                StatusCode::CONFLICT,
            )
            .await
        }
    }
}

#[derive(Deserialize)]
pub struct CsrfOnly {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

pub async fn toggle(
    State(state): State<AppState>,
    Admin(session): Admin,
    headers: HeaderMap,
    Path(id): Path<String>,
    Form(form): Form<CsrfOnly>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let rule = get_rule(&state, id.clone()).await?;
    let enable = !rule.enabled;
    let resolved = state
        .inner
        .db
        .call(move |c| {
            rule_db::set_enabled(c, &id, enable)?;
            if enable {
                let rules = ingest::load_rules(c)?;
                ingest::rerun_inbox(c, &rules, Timestamp::now())
            } else {
                Ok(0)
            }
        })
        .await?;
    let notice = if enable { "enabled" } else { "disabled" };
    Ok(Redirect::to(&format!(
        "/rules/{}?notice={notice}&resolved={resolved}",
        rule.id
    ))
    .into_response())
}

pub async fn delete_sample(
    State(state): State<AppState>,
    Admin(session): Admin,
    headers: HeaderMap,
    Path((id, sample)): Path<(String, i64)>,
    Form(form): Form<CsrfOnly>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let rule = id.clone();
    let found = state
        .inner
        .db
        .call(move |c| rule_db::delete_sample(c, &rule, sample))
        .await?;
    if !found {
        return Err(AppError::NotFound("no such sample".into()));
    }
    Ok(Redirect::to(&format!("/rules/{id}?notice=sample-deleted")).into_response())
}
