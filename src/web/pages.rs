//! Page handlers.

use askama::Template;
use axum::extract::{Query, State};
use axum::response::{Html, IntoResponse, Response};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use super::auth::{AuthMode, Session, User};
use super::views::PackageRow;
use super::{AppError, AppState};
use crate::db::packages::{self, Scope};
use crate::model::{Package, sort_packages};

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

async fn load(state: &AppState, scope: Scope) -> Result<Vec<Package>, AppError> {
    let mut list = state
        .inner
        .db
        .call(move |c| packages::list(c, scope))
        .await?;
    sort_packages(&mut list);
    Ok(list)
}

#[derive(Template)]
#[template(path = "index.html")]
struct IndexPage {
    layout: Layout,
    rows: Vec<PackageRow>,
}

pub async fn index(
    State(state): State<AppState>,
    User(session): User,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let now = state.now();
    let rows = load(&state, Scope::Active)
        .await?
        .iter()
        .map(|p| PackageRow::new(p, now))
        .collect();
    Ok(Html(
        IndexPage {
            layout: Layout::new(&state, &session, &jar, "Packages"),
            rows,
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
    let q = query.q.trim().to_string();
    let scope = if q.is_empty() {
        Scope::Active
    } else {
        Scope::All
    };
    let mut rows: Vec<PackageRow> = load(&state, scope)
        .await?
        .iter()
        .map(|p| PackageRow::new(p, now))
        .filter(|r| q.is_empty() || r.matches(&q))
        .collect();
    rows.truncate(8);
    Ok(Html(SearchResults { q, rows }.render()?).into_response())
}
