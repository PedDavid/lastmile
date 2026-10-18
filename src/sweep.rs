//! Retention, run in the background: archive packages delivered a while
//! ago, and empty the text of old messages (personal data).

use jiff::{SignedDuration, Timestamp};
use rusqlite::{Connection, params};

use crate::db::{Db, messages, secs};

#[derive(Clone, Copy, Debug)]
pub struct Retention {
    /// Delivered packages are archived this long after delivery.
    pub archive_after: SignedDuration,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Swept {
    pub archived: usize,
    pub purged: usize,
}

pub fn sweep(conn: &Connection, r: Retention, now: Timestamp) -> rusqlite::Result<Swept> {
    let archived = conn.execute(
        "UPDATE packages SET archived_at = ?1 WHERE archived_at IS NULL \
         AND status = 'delivered' AND status_at <= ?2",
        params![secs(now), secs(now - r.archive_after)],
    )?;
    let purged = messages::purge(conn, now)?;
    Ok(Swept { archived, purged })
}

/// Sweeps now and then every hour, for as long as the process runs.
pub async fn run(db: Db, r: Retention) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(3600));
    loop {
        tick.tick().await;
        match db.call(move |c| sweep(c, r, Timestamp::now())).await {
            Ok(s) if s != Swept::default() => {
                tracing::info!(archived = s.archived, purged = s.purged, "retention sweep")
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "retention sweep failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::packages::{self, Scope};

    #[test]
    fn archives_delivered_packages_after_a_while() {
        let db = Db::open_in_memory().unwrap();
        let now: Timestamp = "2026-10-18T12:00:00Z".parse().unwrap();
        db.blocking(|c| crate::demo::load(c, now)).unwrap();
        let r = Retention {
            archive_after: SignedDuration::from_hours(24),
        };
        // The printer ink was delivered 55h ago.
        let swept = db.blocking(|c| sweep(c, r, now)).unwrap();
        assert_eq!(swept.archived, 1);
        let archived = db.blocking(|c| packages::list(c, Scope::Archived)).unwrap();
        assert!(archived.iter().any(|p| p.title == "Printer ink"));
        assert_eq!(db.blocking(|c| sweep(c, r, now)).unwrap(), Swept::default());
    }
}
