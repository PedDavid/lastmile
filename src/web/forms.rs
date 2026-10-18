//! Adding and editing packages by hand, overriding their status and
//! archiving them. Every logged-in household member may do all of it.

use askama::Template;
use axum::Form;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use serde::Deserialize;

use super::auth::User;
use super::pages::Layout;
use super::views::fmt_date;
use super::{AppError, AppState};
use crate::couriers;
use crate::db::messages;
use crate::db::packages::{self, Suggest};
use crate::ingest;
use crate::model::{CodeKind, NewPackage, Package, PickupPoint, Source, Status};

/// The package form's fields, as typed.
#[derive(Deserialize, Default, Clone)]
pub struct PackageForm {
    #[serde(rename = "_csrf")]
    pub csrf: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub merchant: String,
    #[serde(default)]
    pub recipient: String,
    #[serde(default)]
    pub courier: String,
    #[serde(default)]
    pub tracking_number: String,
    #[serde(default)]
    pub order_ref: String,
    #[serde(default)]
    pub tracking_url: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub eta_from: String,
    #[serde(default)]
    pub eta_to: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub code_kind: String,
    #[serde(default)]
    pub code_expires_at: String,
    #[serde(default)]
    pub pickup_name: String,
    #[serde(default)]
    pub pickup_address: String,
    #[serde(default)]
    pub pickup_hours: String,
    #[serde(default)]
    pub notes: String,
    /// The inbox message this package is created from.
    #[serde(default)]
    pub message_id: Option<i64>,
}

fn opt(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Parses an `<input type="date">` value as midnight UTC.
pub fn parse_date(value: &str) -> Result<Option<Timestamp>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let date: Date = value
        .parse()
        .map_err(|_| format!("invalid date {value:?}"))?;
    date.to_zoned(TimeZone::UTC)
        .map(|z| Some(z.timestamp()))
        .map_err(|e| e.to_string())
}

const MAX_FIELD: usize = 500;
const MAX_NOTES: usize = 4000;

impl PackageForm {
    fn from_package(p: &Package) -> PackageForm {
        PackageForm::from_new(&p.to_new())
    }

    fn from_new(p: &NewPackage) -> PackageForm {
        let s = |v: &Option<String>| v.clone().unwrap_or_default();
        let d = |v: Option<Timestamp>| v.map(fmt_date).unwrap_or_default();
        PackageForm {
            csrf: None,
            title: p.title.clone(),
            merchant: s(&p.merchant),
            recipient: s(&p.recipient),
            courier: s(&p.courier),
            tracking_number: s(&p.tracking_number),
            order_ref: s(&p.order_ref),
            tracking_url: s(&p.tracking_url),
            status: p.status.as_str().into(),
            eta_from: d(p.eta_from),
            eta_to: d(p.eta_to),
            code: s(&p.code),
            code_kind: p
                .code_kind
                .map(|k| k.as_str().to_string())
                .unwrap_or_default(),
            code_expires_at: d(p.code_expires_at),
            pickup_name: s(&p.pickup_point.name),
            pickup_address: s(&p.pickup_point.address),
            pickup_hours: s(&p.pickup_point.hours),
            notes: s(&p.notes),
            message_id: None,
        }
    }

    /// Checks the fields and builds the package. The courier is filled in
    /// from the tracking number's format when left empty.
    pub fn parse(&self, now: Timestamp) -> Result<NewPackage, String> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err("Give the package a name.".into());
        }
        let long = [
            &self.title,
            &self.merchant,
            &self.recipient,
            &self.courier,
            &self.tracking_number,
            &self.order_ref,
            &self.tracking_url,
            &self.code,
            &self.pickup_name,
            &self.pickup_address,
            &self.pickup_hours,
        ]
        .iter()
        .any(|f| f.len() > MAX_FIELD);
        if long || self.notes.len() > MAX_NOTES {
            return Err("A field is too long.".into());
        }
        let status = match self.status.trim() {
            "" => Status::Announced,
            s => Status::parse(s).ok_or_else(|| format!("unknown status {s:?}"))?,
        };
        let code_kind = match self.code_kind.trim() {
            "" => None,
            k => Some(CodeKind::parse(k).ok_or_else(|| format!("unknown code kind {k:?}"))?),
        };
        let tracking_url = opt(&self.tracking_url);
        if let Some(url) = &tracking_url
            && super::views::safe_link(url).is_none()
        {
            return Err("The tracking link must be an http(s) URL.".into());
        }
        let eta_from = parse_date(&self.eta_from)?;
        let eta_to = parse_date(&self.eta_to)?.or(eta_from);
        if let (Some(from), Some(to)) = (eta_from, eta_to)
            && to < from
        {
            return Err("The expected range ends before it starts.".into());
        }
        let tracking_number = opt(&self.tracking_number).map(|t| t.replace(' ', ""));
        let courier = opt(&self.courier).or_else(|| {
            tracking_number
                .as_deref()
                .and_then(couriers::detect)
                .map(str::to_string)
        });
        Ok(NewPackage {
            title: title.to_string(),
            merchant: opt(&self.merchant),
            recipient: opt(&self.recipient),
            courier,
            tracking_number,
            order_ref: opt(&self.order_ref),
            tracking_url,
            status,
            status_source: Source::Manual,
            status_at: now,
            eta_from,
            eta_to,
            code: opt(&self.code),
            code_kind: code_kind.or_else(|| opt(&self.code).map(|_| CodeKind::Delivery)),
            code_expires_at: parse_date(&self.code_expires_at)?,
            pickup_point: PickupPoint {
                name: opt(&self.pickup_name),
                address: opt(&self.pickup_address),
                hours: opt(&self.pickup_hours),
            },
            notes: opt(&self.notes),
        })
    }
}

#[derive(Template)]
#[template(path = "package_form.html")]
struct FormPage {
    layout: Layout,
    /// `None` when adding.
    id: Option<i64>,
    form: PackageForm,
    error: String,
    statuses: Vec<(&'static str, &'static str)>,
    couriers: Vec<String>,
    recipients: Vec<String>,
    merchants: Vec<String>,
}

impl FormPage {
    fn action(&self) -> String {
        match self.id {
            Some(id) => format!("/packages/{id}"),
            None => "/packages".into(),
        }
    }
}

async fn form_page(
    state: &AppState,
    user: &User,
    jar: &CookieJar,
    id: Option<i64>,
    form: PackageForm,
    error: String,
) -> Result<Response, AppError> {
    let (couriers, recipients, merchants) = state
        .inner
        .db
        .call(|c| {
            Ok((
                packages::distinct(c, Suggest::Courier)?,
                packages::distinct(c, Suggest::Recipient)?,
                packages::distinct(c, Suggest::Merchant)?,
            ))
        })
        .await?;
    let title = if id.is_some() {
        "Edit package"
    } else {
        "Add a package"
    };
    let page = FormPage {
        layout: Layout::new(state, &user.0, jar, title),
        id,
        form,
        error,
        statuses: Status::ALL
            .iter()
            .map(|s| (s.as_str(), s.label()))
            .collect(),
        couriers,
        recipients,
        merchants,
    };
    let status = if page.error.is_empty() {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::UNPROCESSABLE_ENTITY
    };
    Ok((status, Html(page.render()?)).into_response())
}

async fn get_package(state: &AppState, id: i64) -> Result<Package, AppError> {
    state
        .inner
        .db
        .call(move |c| packages::get(c, id))
        .await?
        .ok_or_else(|| AppError::NotFound(format!("no package {id}")))
}

#[derive(Deserialize, Default)]
pub struct NewQuery {
    /// Prefill from what a rule read in this inbox message.
    message: Option<i64>,
}

pub async fn new(
    State(state): State<AppState>,
    user: User,
    jar: CookieJar,
    Query(q): Query<NewQuery>,
) -> Result<Response, AppError> {
    let mut form = PackageForm::default();
    if let Some(id) = q.message {
        let m = state
            .inner
            .db
            .call(move |c| messages::get(c, id))
            .await?
            .ok_or_else(|| AppError::NotFound(format!("no message {id}")))?;
        let mut p = NewPackage::titled(String::new(), m.sent_at);
        ingest::merge_fields(&mut p, &m.extracted);
        if p.title.is_empty() && !m.extracted.is_empty() {
            p.title = ingest::default_title(&p);
        }
        p.status = m
            .extracted
            .get("status")
            .and_then(|s| Status::parse(s))
            .unwrap_or(Status::Announced);
        form = PackageForm::from_new(&p);
        form.message_id = Some(id);
    }
    form_page(&state, &user, &jar, None, form, String::new()).await
}

pub async fn create(
    State(state): State<AppState>,
    user: User,
    jar: CookieJar,
    headers: HeaderMap,
    Form(form): Form<PackageForm>,
) -> Result<Response, AppError> {
    user.0.check_csrf(&headers, form.csrf.as_deref())?;
    let now = Timestamp::now();
    let package = match form.parse(now) {
        Ok(p) => p,
        Err(e) => return form_page(&state, &user, &jar, None, form, e).await,
    };
    let description = format!("Added by {}", user.0.name);
    let message = form.message_id;
    let id = state
        .inner
        .db
        .call(move |c| {
            let id = packages::insert(c, &package, &description, now)?;
            if let Some(m) = message {
                messages::attach(c, m, id)?;
            }
            Ok(id)
        })
        .await?;
    Ok(Redirect::to(&format!("/packages/{id}?notice=added")).into_response())
}

pub async fn edit(
    State(state): State<AppState>,
    user: User,
    jar: CookieJar,
    Path(id): Path<i64>,
) -> Result<Response, AppError> {
    let p = get_package(&state, id).await?;
    form_page(
        &state,
        &user,
        &jar,
        Some(id),
        PackageForm::from_package(&p),
        String::new(),
    )
    .await
}

pub async fn update(
    State(state): State<AppState>,
    user: User,
    jar: CookieJar,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(form): Form<PackageForm>,
) -> Result<Response, AppError> {
    user.0.check_csrf(&headers, form.csrf.as_deref())?;
    let now = Timestamp::now();
    let package = match form.parse(now) {
        Ok(p) => p,
        Err(e) => return form_page(&state, &user, &jar, Some(id), form, e).await,
    };
    let description = format!("Details edited by {}", user.0.name);
    let found = state
        .inner
        .db
        .call(move |c| packages::update(c, id, &package, &description, now))
        .await?;
    if !found {
        return Err(AppError::NotFound(format!("no package {id}")));
    }
    Ok(Redirect::to(&format!("/packages/{id}?notice=saved")).into_response())
}

#[derive(Deserialize)]
pub struct StatusForm {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
    status: String,
    #[serde(default)]
    note: String,
}

/// Sets the status by hand. Manual changes always apply, even backwards.
pub async fn set_status(
    State(state): State<AppState>,
    User(session): User,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(form): Form<StatusForm>,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let status = Status::parse(&form.status)
        .ok_or_else(|| AppError::BadRequest(format!("unknown status {:?}", form.status)))?;
    let note = form.note.trim();
    if note.len() > MAX_FIELD {
        return Err(AppError::BadRequest("the note is too long".into()));
    }
    let description = if note.is_empty() {
        format!("{} (set by {})", status.label(), session.name)
    } else {
        format!("{note} (set by {})", session.name)
    };
    let found = state
        .inner
        .db
        .call(move |c| {
            packages::set_status(
                c,
                id,
                status,
                Source::Manual,
                Timestamp::now(),
                &description,
            )
        })
        .await?;
    if !found {
        return Err(AppError::NotFound(format!("no package {id}")));
    }
    Ok(Redirect::to(&format!("/packages/{id}?notice=status")).into_response())
}

#[derive(Deserialize)]
pub struct CsrfOnly {
    #[serde(rename = "_csrf")]
    csrf: Option<String>,
}

async fn archive_as(
    state: AppState,
    session: super::auth::Session,
    headers: HeaderMap,
    id: i64,
    form: CsrfOnly,
    archived: bool,
) -> Result<Response, AppError> {
    session.check_csrf(&headers, form.csrf.as_deref())?;
    let at = archived.then(Timestamp::now);
    let found = state
        .inner
        .db
        .call(move |c| packages::set_archived(c, id, at))
        .await?;
    if !found {
        return Err(AppError::NotFound(format!("no package {id}")));
    }
    let notice = if archived { "archived" } else { "unarchived" };
    Ok(Redirect::to(&format!("/packages/{id}?notice={notice}")).into_response())
}

pub async fn archive(
    State(state): State<AppState>,
    User(session): User,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(form): Form<CsrfOnly>,
) -> Result<Response, AppError> {
    archive_as(state, session, headers, id, form, true).await
}

pub async fn unarchive(
    State(state): State<AppState>,
    User(session): User,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(form): Form<CsrfOnly>,
) -> Result<Response, AppError> {
    archive_as(state, session, headers, id, form, false).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        "2026-10-18T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn a_title_is_all_it_takes() {
        let p = PackageForm {
            title: "  Shoes ".into(),
            ..Default::default()
        }
        .parse(now())
        .unwrap();
        assert_eq!(p.title, "Shoes");
        assert_eq!(p.status, Status::Announced);
        assert_eq!(p.status_source, Source::Manual);
        assert_eq!(p.courier, None);
        assert!(PackageForm::default().parse(now()).is_err());
    }

    #[test]
    fn the_courier_comes_from_the_tracking_number() {
        let form = PackageForm {
            title: "Shoes".into(),
            tracking_number: "RR 123456789 PT".into(),
            code: "482913".into(),
            ..Default::default()
        };
        let p = form.parse(now()).unwrap();
        assert_eq!(p.tracking_number.as_deref(), Some("RR123456789PT"));
        assert_eq!(p.courier.as_deref(), Some("CTT"));
        // A code without a kind is for the door.
        assert_eq!(p.code_kind, Some(CodeKind::Delivery));
        // A courier typed in wins.
        let p = PackageForm {
            courier: "CTT Expresso".into(),
            ..form
        }
        .parse(now())
        .unwrap();
        assert_eq!(p.courier.as_deref(), Some("CTT Expresso"));
    }

    #[test]
    fn dates_and_links_are_checked() {
        let base = PackageForm {
            title: "x".into(),
            ..Default::default()
        };
        let p = PackageForm {
            eta_from: "2026-10-20".into(),
            ..base.clone()
        }
        .parse(now())
        .unwrap();
        assert_eq!(p.eta_to, p.eta_from, "a single day");
        assert!(
            PackageForm {
                eta_from: "2026-10-20".into(),
                eta_to: "2026-10-19".into(),
                ..base.clone()
            }
            .parse(now())
            .is_err()
        );
        assert!(
            PackageForm {
                tracking_url: "javascript:alert(1)".into(),
                ..base.clone()
            }
            .parse(now())
            .is_err()
        );
        assert!(
            PackageForm {
                eta_from: "20/10/2026".into(),
                ..base
            }
            .parse(now())
            .is_err()
        );
    }
}
