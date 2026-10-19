//! Checking packages with their courier, in the background: the courier's
//! tracking page is opened in a headless browser (it is a web app, with
//! nothing in its HTML), and the events on it become the package's timeline
//! and, when they are news, its status. Only packages the courier issued a
//! tracking number for are checked, one at a time, and with a pause between
//! them.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use jiff::Timestamp;
use rusqlite::{Connection, params};

use crate::browser::{self, BrowserError};
use crate::couriers::{self, ctt};
use crate::db::{Db, packages, secs};
use crate::metrics::Metrics;
use crate::model::{Package, Source, Status};

/// Opens a tracking page: the page's address, then the tracking number.
/// Returns what a visitor would read on it.
pub type Fetch =
    Arc<dyn Fn(String, String) -> BoxFuture<'static, Result<String, BrowserError>> + Send + Sync>;

/// Reads pages with a real browser.
pub fn browser_fetch(client: Arc<browser::Client>) -> Fetch {
    Arc::new(move |url, _tracking| {
        let client = client.clone();
        Box::pin(async move { client.render(&url, ctt::drawn).await })
    })
}

const COURIER: &str = "CTT";

/// Packages worth asking CTT about: its tracking numbers, not delivered.
fn candidates(conn: &Connection) -> rusqlite::Result<Vec<Package>> {
    Ok(packages::list(conn, packages::Scope::Active)?
        .into_iter()
        .filter(|p| p.status != Status::Delivered)
        .filter(|p| {
            p.tracking_number.as_deref().is_some_and(|t| {
                couriers::detect(t) == Some(COURIER)
                    || p.courier
                        .as_deref()
                        .is_some_and(|c| c.eq_ignore_ascii_case(COURIER))
            })
        })
        .collect())
}

/// Adds what the page says to the package: events not on the timeline yet,
/// the status when it is news, the expected day, and the link to the page.
/// Returns how many events were new.
fn apply(
    conn: &mut Connection,
    id: i64,
    events: &[ctt::Event],
    eta: Option<Timestamp>,
    url: &str,
) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    let known: HashSet<(i64, String)> = packages::events(&tx, id)?
        .into_iter()
        .filter(|e| e.source == Source::Courier)
        .map(|e| (e.at.as_second(), e.description))
        .collect();
    let mut added = 0;
    for e in events {
        if known.contains(&(e.at.as_second(), e.description.clone())) {
            continue;
        }
        let Some(p) = packages::get(&tx, id)? else {
            return Ok(added);
        };
        match e.status {
            Some(status) if p.accepts(status, e.at) => {
                packages::set_status_in(&tx, id, status, Source::Courier, e.at, &e.description)?;
            }
            status => {
                packages::insert_event(&tx, id, e.at, status, &e.description, Source::Courier)?;
            }
        }
        added += 1;
    }
    if let Some(eta) = eta {
        tx.execute(
            "UPDATE packages SET eta_from = ?2, eta_to = ?2 WHERE id = ?1 AND status != 'delivered'",
            params![id, secs(eta)],
        )?;
    }
    tx.execute(
        "UPDATE packages SET tracking_url = ?2 WHERE id = ?1 AND tracking_url IS NULL",
        params![id, url],
    )?;
    tx.commit()?;
    Ok(added)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub checked: usize,
    pub updated: usize,
    pub empty: usize,
    pub failed: usize,
}

/// Checks every candidate package once, `pause` apart.
pub async fn poll_once(db: &Db, fetch: &Fetch, metrics: &Metrics, pause: Duration) -> Report {
    let mut report = Report::default();
    let list = match db.call(|c| candidates(c)).await {
        Ok(list) => list,
        Err(e) => {
            tracing::error!(error = %e, "could not list the packages to check");
            return report;
        }
    };
    for (n, p) in list.iter().enumerate() {
        if n > 0 {
            tokio::time::sleep(pause).await;
        }
        let tracking = p.tracking_number.clone().unwrap_or_default();
        let url = ctt::tracking_url(&tracking);
        report.checked += 1;
        let text = match fetch(url.clone(), tracking).await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(package = p.id, error = %e, "could not read the tracking page");
                metrics.courier_check(COURIER, "error");
                report.failed += 1;
                continue;
            }
        };
        let events = ctt::parse(&text);
        if events.is_empty() {
            // A page that loaded but says nothing we know how to read: a new
            // layout, or an object CTT doesn't know (yet).
            tracing::info!(package = p.id, "the tracking page has no events to read");
            tracing::debug!(package = p.id, text = %truncate(&text, 4000), "tracking page text");
            metrics.courier_check(COURIER, "empty");
            report.empty += 1;
            continue;
        }
        let (id, eta) = (p.id, ctt::eta(&text));
        match db.call(move |c| apply(c, id, &events, eta, &url)).await {
            Ok(added) => {
                metrics.courier_check(COURIER, "ok");
                if added > 0 {
                    tracing::info!(package = id, events = added, "tracking page had news");
                    report.updated += 1;
                }
            }
            Err(e) => {
                tracing::error!(package = id, error = %e, "could not save the tracking events");
                metrics.courier_check(COURIER, "error");
                report.failed += 1;
            }
        }
    }
    report
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Checks the packages now and then every `interval`, for as long as the
/// process runs. The first check waits a minute, for the app to settle.
pub async fn run(db: Db, fetch: Fetch, metrics: Arc<Metrics>, interval: Duration) {
    tokio::time::sleep(Duration::from_secs(60)).await;
    let mut tick = tokio::time::interval(interval);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let report = poll_once(&db, &fetch, &metrics, Duration::from_secs(5)).await;
        if report.checked > 0 {
            tracing::info!(
                checked = report.checked,
                updated = report.updated,
                empty = report.empty,
                failed = report.failed,
                "checked packages with the courier"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NewPackage;
    use std::sync::Mutex;

    const NOW: &str = "2026-10-19T12:00:00Z";
    const PAGE: &str = "Rastreio\nDW000000001PT\nEntrega prevista a 21-10-2026\n\
        16-10-2026 09:30 Objeto aceite\n\
        18-10-2026 17:40 Em distribuição\n";

    fn now() -> Timestamp {
        NOW.parse().unwrap()
    }

    fn package(db: &Db, tracking: Option<&str>, status: Status, source: Source) -> i64 {
        db.blocking(|c| {
            let mut p = NewPackage::titled("Parcel", "2026-10-15T08:00:00Z".parse().unwrap());
            p.tracking_number = tracking.map(str::to_string);
            p.status = status;
            p.status_source = source;
            p.status_at = "2026-10-15T08:00:00Z".parse().unwrap();
            packages::insert_in(c, &p, "test", now())
        })
        .unwrap()
    }

    /// A fetch that answers with `page` (or fails when it is `None`) and
    /// remembers what it was asked.
    fn fetch(page: Option<&'static str>) -> (Fetch, Arc<Mutex<Vec<String>>>) {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let seen = asked.clone();
        let f: Fetch = Arc::new(move |url, _| {
            seen.lock().unwrap().push(url);
            Box::pin(async move {
                page.map(str::to_string)
                    .ok_or(BrowserError::Timeout(Duration::from_secs(1)))
            })
        });
        (f, asked)
    }

    fn run_once(db: &Db, f: &Fetch) -> Report {
        let metrics = Metrics::new(db.clone());
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(poll_once(db, f, &metrics, Duration::ZERO))
    }

    #[test]
    fn the_page_moves_the_package_forwards_and_fills_its_timeline() {
        let db = Db::open_in_memory().unwrap();
        let id = package(
            &db,
            Some("DW000000001PT"),
            Status::Announced,
            Source::Message,
        );
        let (f, asked) = fetch(Some(PAGE));
        let report = run_once(&db, &f);
        assert_eq!(
            report,
            Report {
                checked: 1,
                updated: 1,
                empty: 0,
                failed: 0
            }
        );
        assert_eq!(
            asked.lock().unwrap().as_slice(),
            ["https://www.ctt.pt/t/DW000000001PT"]
        );
        let p = db.blocking(|c| packages::get(c, id)).unwrap().unwrap();
        assert_eq!(p.status, Status::OutForDelivery);
        assert_eq!(p.status_source, Source::Courier);
        assert_eq!(
            p.status_at,
            "2026-10-18T17:40:00Z".parse::<Timestamp>().unwrap()
        );
        assert_eq!(p.eta_from, Some("2026-10-21T00:00:00Z".parse().unwrap()));
        assert_eq!(
            p.tracking_url.as_deref(),
            Some("https://www.ctt.pt/t/DW000000001PT")
        );
        let events = db.blocking(|c| packages::events(c, id)).unwrap();
        let courier: Vec<_> = events
            .iter()
            .filter(|e| e.source == Source::Courier)
            .collect();
        assert_eq!(courier.len(), 2, "{events:?}");
    }

    #[test]
    fn asking_again_adds_nothing() {
        let db = Db::open_in_memory().unwrap();
        let id = package(
            &db,
            Some("DW000000001PT"),
            Status::Announced,
            Source::Message,
        );
        let (f, _) = fetch(Some(PAGE));
        run_once(&db, &f);
        let before = db.blocking(|c| packages::events(c, id)).unwrap().len();
        let report = run_once(&db, &f);
        assert_eq!(report.updated, 0);
        assert_eq!(
            db.blocking(|c| packages::events(c, id)).unwrap().len(),
            before
        );
    }

    #[test]
    fn news_older_than_what_is_known_goes_on_the_timeline_only() {
        let db = Db::open_in_memory().unwrap();
        // Someone set it to ready for pickup on the 19th, after the page's news.
        let id = db
            .blocking(|c| {
                let mut p = NewPackage::titled("Parcel", "2026-10-15T08:00:00Z".parse().unwrap());
                p.tracking_number = Some("DW000000001PT".into());
                p.status = Status::ReadyForPickup;
                p.status_source = Source::Manual;
                p.status_at = "2026-10-19T09:00:00Z".parse().unwrap();
                packages::insert_in(c, &p, "test", now())
            })
            .unwrap();
        let (f, _) = fetch(Some(PAGE));
        run_once(&db, &f);
        let p = db.blocking(|c| packages::get(c, id)).unwrap().unwrap();
        assert_eq!(p.status, Status::ReadyForPickup);
        assert_eq!(p.status_source, Source::Manual);
        let events = db.blocking(|c| packages::events(c, id)).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| e.source == Source::Courier)
                .count(),
            2
        );
    }

    #[test]
    fn only_ctt_packages_that_are_not_delivered_are_asked_about() {
        let db = Db::open_in_memory().unwrap();
        package(&db, Some("PCK7Q2W9D"), Status::InTransit, Source::Message);
        package(&db, None, Status::InTransit, Source::Manual);
        package(
            &db,
            Some("DW000000002PT"),
            Status::Delivered,
            Source::Message,
        );
        let (f, asked) = fetch(Some(PAGE));
        let report = run_once(&db, &f);
        assert_eq!(report, Report::default());
        assert!(asked.lock().unwrap().is_empty());
    }

    #[test]
    fn a_page_that_cannot_be_read_or_has_nothing_to_read_changes_nothing() {
        let db = Db::open_in_memory().unwrap();
        let id = package(
            &db,
            Some("DW000000001PT"),
            Status::InTransit,
            Source::Message,
        );
        let (broken, _) = fetch(None);
        assert_eq!(
            run_once(&db, &broken),
            Report {
                checked: 1,
                updated: 0,
                empty: 0,
                failed: 1
            }
        );
        let (blank, _) = fetch(Some("Rastreio de objetos\nDW000000001PT"));
        assert_eq!(
            run_once(&db, &blank),
            Report {
                checked: 1,
                updated: 0,
                empty: 1,
                failed: 0
            }
        );
        let p = db.blocking(|c| packages::get(c, id)).unwrap().unwrap();
        assert_eq!(p.status, Status::InTransit);
        assert_eq!(p.status_source, Source::Message);
        assert!(
            db.blocking(|c| packages::events(c, id))
                .unwrap()
                .iter()
                .all(|e| e.source != Source::Courier)
        );
    }
}
