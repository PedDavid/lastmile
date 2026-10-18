//! Page handlers.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use super::auth::{AuthMode, Session, User};
use super::views::{PackageDetail, PackageRow};
use super::{AppError, AppState};
use crate::db::messages;
use crate::db::packages::{self, Scope};
use crate::model::{Filter, Package, Status, sort_packages};

/// Data shared by every full page.
pub struct Layout {
    pub title: String,
    pub user: String,
    pub admin: bool,
    pub csrf: String,
    pub asset_version: String,
    pub dark: bool,
    pub palette: String,
    pub dev_auth: bool,
}

pub const PALETTES: [&str; 5] = ["neutral", "blue", "green", "orange", "violet"];

impl Layout {
    pub fn new(
        state: &AppState,
        session: &Session,
        jar: &CookieJar,
        title: impl Into<String>,
    ) -> Layout {
        let palette = jar
            .get("lastmile_palette")
            .map(|c| c.value().to_string())
            .filter(|p| PALETTES.contains(&p.as_str()))
            .unwrap_or_else(|| "neutral".into());
        Layout {
            title: title.into(),
            user: session.name.clone(),
            admin: session.admin,
            csrf: session.csrf.clone(),
            asset_version: state.inner.asset_version.clone(),
            dark: jar
                .get("lastmile_theme")
                .is_some_and(|c| c.value() == "dark"),
            palette,
            dev_auth: matches!(state.inner.auth, AuthMode::InsecureDev { .. }),
        }
    }
}

pub async fn load(state: &AppState, scope: Scope) -> Result<Vec<Package>, AppError> {
    let mut list = state
        .inner
        .db
        .call(move |c| packages::list(c, scope))
        .await?;
    sort_packages(&mut list);
    Ok(list)
}

fn is_htmx(headers: &HeaderMap, target: &str) -> bool {
    headers.contains_key("hx-request")
        && headers
            .get("hx-target")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t == target)
}

#[derive(Deserialize, Default, Clone)]
pub struct Filters {
    #[serde(default)]
    pub q: String,
    /// One of [`Filter`]'s names; empty for every active package.
    #[serde(default)]
    pub show: String,
    #[serde(default)]
    pub recipient: String,
}

pub struct FilterCount {
    pub name: &'static str,
    pub label: &'static str,
    pub count: usize,
    pub active: bool,
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexPage {
    layout: Layout,
    rows: Vec<PackageRow>,
    total: usize,
    active: usize,
    counts: Vec<FilterCount>,
    recipients: Vec<String>,
    filters: Filters,
    oob: bool,
}

#[derive(Template)]
#[template(path = "partials/packages.html")]
struct PackagesPartial {
    rows: Vec<PackageRow>,
    total: usize,
    active: usize,
    counts: Vec<FilterCount>,
    filters: Filters,
    /// Also re-render the filter chips as an htmx out-of-band swap.
    oob: bool,
}

pub async fn index(
    State(state): State<AppState>,
    User(session): User,
    jar: CookieJar,
    headers: HeaderMap,
    Query(filters): Query<Filters>,
) -> Result<Response, AppError> {
    let now = state.now();
    let stale = state.inner.cfg.stale_after;
    let all = load(&state, Scope::All).await?;
    let total = all.len();
    let wanted = Filter::parse(&filters.show);

    let mut recipients: Vec<String> = all.iter().filter_map(|p| p.recipient.clone()).collect();
    recipients.sort_by_key(|r| r.to_lowercase());
    recipients.dedup();

    let searched: Vec<(&Package, PackageRow)> = all
        .iter()
        .map(|p| (p, PackageRow::new(p, now, stale)))
        .filter(|(_, r)| filters.q.is_empty() || r.matches(&filters.q))
        .filter(|(_, r)| filters.recipient.is_empty() || r.recipient == filters.recipient)
        .collect();
    let in_view = |p: &Package, r: &PackageRow, f: Option<Filter>| match f {
        None => p.archived_at.is_none(),
        Some(Filter::Attention) => !r.attention.is_empty(),
        Some(f) => p.filter() == Some(f),
    };
    let active = searched.iter().filter(|(p, r)| in_view(p, r, None)).count();
    let counts = Filter::ALL
        .iter()
        .map(|f| FilterCount {
            name: f.as_str(),
            label: f.label(),
            count: searched
                .iter()
                .filter(|(p, r)| in_view(p, r, Some(*f)))
                .count(),
            active: wanted == Some(*f),
        })
        .collect();
    let rows: Vec<PackageRow> = searched
        .into_iter()
        .filter(|(p, r)| in_view(p, r, wanted))
        .map(|(_, r)| r)
        .collect();

    if is_htmx(&headers, "packages") {
        return Ok(Html(
            PackagesPartial {
                rows,
                total,
                active,
                counts,
                filters,
                oob: true,
            }
            .render()?,
        )
        .into_response());
    }
    Ok(Html(
        IndexPage {
            layout: Layout::new(&state, &session, &jar, "Packages"),
            rows,
            total,
            active,
            counts,
            recipients,
            filters,
            oob: false,
        }
        .render()?,
    )
    .into_response())
}

#[derive(Template)]
#[template(path = "detail.html")]
struct DetailPage {
    layout: Layout,
    p: PackageDetail,
    messages: Vec<LinkedMessage>,
    statuses: Vec<(&'static str, &'static str)>,
    notice: &'static str,
}

pub struct LinkedMessage {
    pub channel: &'static str,
    pub received: String,
    /// Empty once purged.
    pub text: String,
}

#[derive(Deserialize, Default)]
pub struct DetailQuery {
    #[serde(default)]
    notice: String,
}

pub async fn detail(
    State(state): State<AppState>,
    User(session): User,
    jar: CookieJar,
    Path(id): Path<i64>,
    Query(q): Query<DetailQuery>,
) -> Result<Response, AppError> {
    let (package, events, linked) = state
        .inner
        .db
        .call(move |c| {
            let p = packages::get(c, id)?;
            let e = packages::events(c, id)?;
            let m = messages::for_package(c, id)?;
            Ok((p, e, m))
        })
        .await?;
    let package = package.ok_or_else(|| AppError::NotFound(format!("no package {id}")))?;
    let now = state.now();
    let p = PackageDetail::new(&package, &events, now, state.inner.cfg.stale_after);
    let messages = linked
        .iter()
        .map(|m| LinkedMessage {
            channel: m.channel.label(),
            received: super::views::relative(m.received_at, now),
            text: m.text().unwrap_or_default(),
        })
        .collect();
    let notice = match q.notice.as_str() {
        "added" => "Package added.",
        "saved" => "Changes saved.",
        "status" => "Status updated.",
        "archived" => "Archived. It stays searchable.",
        "unarchived" => "Back in the list.",
        "from-message" => "Package created from the message.",
        "updated-from-message" => "Package updated from the message.",
        "attached" => "Message attached.",
        _ => "",
    };
    Ok(Html(
        DetailPage {
            layout: Layout::new(&state, &session, &jar, p.row.title.clone()),
            statuses: Status::ALL
                .iter()
                .map(|s| (s.as_str(), s.label()))
                .collect(),
            notice,
            p,
            messages,
        }
        .render()?,
    )
    .into_response())
}

#[derive(Deserialize, Default)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
}

#[derive(Template)]
#[template(path = "partials/search_results.html")]
struct SearchResults {
    q: String,
    rows: Vec<PackageRow>,
}

/// Results for the command palette: matches for `q` (archived packages
/// included), or the packages that need something now.
pub async fn search(
    State(state): State<AppState>,
    User(_): User,
    Query(query): Query<SearchQuery>,
) -> Result<Response, AppError> {
    let now = state.now();
    let stale = state.inner.cfg.stale_after;
    let q = query.q.trim().to_string();
    let scope = if q.is_empty() {
        Scope::Active
    } else {
        Scope::All
    };
    let mut rows: Vec<PackageRow> = load(&state, scope)
        .await?
        .iter()
        .map(|p| PackageRow::new(p, now, stale))
        .filter(|r| q.is_empty() || r.matches(&q))
        .collect();
    rows.truncate(8);
    Ok(Html(SearchResults { q, rows }.render()?).into_response())
}
