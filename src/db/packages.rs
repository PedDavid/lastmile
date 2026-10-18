//! Queries on `packages` and `events`.

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{opt_ts, parse_col, secs, ts};
use crate::model::{CodeKind, Event, NewPackage, Package, PickupPoint, Source, Status};

const COLUMNS: &str = "id, title, merchant, recipient, courier, tracking_number, order_ref, \
    tracking_url, status, status_source, status_at, eta_from, eta_to, code, code_kind, \
    code_expires_at, pickup_name, pickup_address, pickup_hours, notes, created_at, archived_at";

fn package(row: &Row) -> rusqlite::Result<Package> {
    Ok(Package {
        id: row.get(0)?,
        title: row.get(1)?,
        merchant: row.get(2)?,
        recipient: row.get(3)?,
        courier: row.get(4)?,
        tracking_number: row.get(5)?,
        order_ref: row.get(6)?,
        tracking_url: row.get(7)?,
        status: parse_col(row.get(8)?, Status::parse)?,
        status_source: parse_col(row.get(9)?, Source::parse)?,
        status_at: ts(row.get(10)?)?,
        eta_from: opt_ts(row.get(11)?)?,
        eta_to: opt_ts(row.get(12)?)?,
        code: row.get(13)?,
        code_kind: row
            .get::<_, Option<String>>(14)?
            .map(|k| parse_col(k, CodeKind::parse))
            .transpose()?,
        code_expires_at: opt_ts(row.get(15)?)?,
        pickup_point: PickupPoint {
            name: row.get(16)?,
            address: row.get(17)?,
            hours: row.get(18)?,
        },
        notes: row.get(19)?,
        created_at: ts(row.get(20)?)?,
        archived_at: opt_ts(row.get(21)?)?,
    })
}

/// Which packages to list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Active,
    Archived,
    All,
}

pub fn list(conn: &Connection, scope: Scope) -> rusqlite::Result<Vec<Package>> {
    let filter = match scope {
        Scope::Active => "WHERE archived_at IS NULL",
        Scope::Archived => "WHERE archived_at IS NOT NULL",
        Scope::All => "",
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM packages {filter} ORDER BY id"
    ))?;
    stmt.query_map([], package)?.collect()
}

pub fn get(conn: &Connection, id: i64) -> rusqlite::Result<Option<Package>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM packages WHERE id = ?1"),
        [id],
        package,
    )
    .optional()
}

/// Inserts a package and the event that created it.
pub fn insert(
    conn: &mut Connection,
    p: &NewPackage,
    description: &str,
    now: Timestamp,
) -> rusqlite::Result<i64> {
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO packages (title, merchant, recipient, courier, tracking_number, order_ref, \
         tracking_url, status, status_source, status_at, eta_from, eta_to, code, code_kind, \
         code_expires_at, pickup_name, pickup_address, pickup_hours, notes, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
        params![
            p.title,
            p.merchant,
            p.recipient,
            p.courier,
            p.tracking_number,
            p.order_ref,
            p.tracking_url,
            p.status.as_str(),
            p.status_source.as_str(),
            secs(p.status_at),
            p.eta_from.map(secs),
            p.eta_to.map(secs),
            p.code,
            p.code_kind.map(CodeKind::as_str),
            p.code_expires_at.map(secs),
            p.pickup_point.name,
            p.pickup_point.address,
            p.pickup_point.hours,
            p.notes,
            secs(now),
        ],
    )?;
    let id = tx.last_insert_rowid();
    insert_event(
        &tx,
        id,
        p.status_at,
        Some(p.status),
        description,
        p.status_source,
    )?;
    tx.commit()?;
    Ok(id)
}

pub fn insert_event(
    conn: &Connection,
    package_id: i64,
    at: Timestamp,
    status: Option<Status>,
    description: &str,
    source: Source,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO events (package_id, at, status, description, source) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            package_id,
            secs(at),
            status.map(Status::as_str),
            description,
            source.as_str()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Replaces a package's details (everything but its status, which only
/// changes through [`set_status`]) and records who changed them.
pub fn update(
    conn: &mut Connection,
    id: i64,
    p: &NewPackage,
    description: &str,
    now: Timestamp,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    let n = tx.execute(
        "UPDATE packages SET title = ?2, merchant = ?3, recipient = ?4, courier = ?5, \
         tracking_number = ?6, order_ref = ?7, tracking_url = ?8, eta_from = ?9, eta_to = ?10, \
         code = ?11, code_kind = ?12, code_expires_at = ?13, pickup_name = ?14, \
         pickup_address = ?15, pickup_hours = ?16, notes = ?17 WHERE id = ?1",
        params![
            id,
            p.title,
            p.merchant,
            p.recipient,
            p.courier,
            p.tracking_number,
            p.order_ref,
            p.tracking_url,
            p.eta_from.map(secs),
            p.eta_to.map(secs),
            p.code,
            p.code_kind.map(CodeKind::as_str),
            p.code_expires_at.map(secs),
            p.pickup_point.name,
            p.pickup_point.address,
            p.pickup_point.hours,
            p.notes,
        ],
    )?;
    if n == 1 {
        insert_event(&tx, id, now, None, description, Source::Manual)?;
    }
    tx.commit()?;
    Ok(n == 1)
}

/// Sets a package's status and adds it to the timeline.
pub fn set_status(
    conn: &mut Connection,
    id: i64,
    status: Status,
    source: Source,
    at: Timestamp,
    description: &str,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    let n = tx.execute(
        "UPDATE packages SET status = ?2, status_source = ?3, status_at = ?4 WHERE id = ?1",
        params![id, status.as_str(), source.as_str(), secs(at)],
    )?;
    if n == 1 {
        insert_event(&tx, id, at, Some(status), description, source)?;
    }
    tx.commit()?;
    Ok(n == 1)
}

/// Distinct non-empty values of a column, for the form's suggestions.
pub fn distinct(conn: &Connection, column: Suggest) -> rusqlite::Result<Vec<String>> {
    let column = match column {
        Suggest::Courier => "courier",
        Suggest::Recipient => "recipient",
        Suggest::Merchant => "merchant",
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT {column} FROM packages WHERE {column} IS NOT NULL AND {column} != '' \
         ORDER BY {column} COLLATE NOCASE"
    ))?;
    stmt.query_map([], |r| r.get(0))?.collect()
}

#[derive(Clone, Copy, Debug)]
pub enum Suggest {
    Courier,
    Recipient,
    Merchant,
}

/// A package's timeline, newest first.
pub fn events(conn: &Connection, package_id: i64) -> rusqlite::Result<Vec<Event>> {
    let mut stmt = conn.prepare(
        "SELECT id, package_id, at, status, description, source FROM events \
         WHERE package_id = ?1 ORDER BY at DESC, id DESC",
    )?;
    stmt.query_map([package_id], |row| {
        Ok(Event {
            id: row.get(0)?,
            package_id: row.get(1)?,
            at: ts(row.get(2)?)?,
            status: row
                .get::<_, Option<String>>(3)?
                .map(|s| parse_col(s, Status::parse))
                .transpose()?,
            description: row.get(4)?,
            source: parse_col(row.get(5)?, Source::parse)?,
        })
    })?
    .collect()
}

/// Archives a package now (or `at`).
pub fn set_archived(conn: &Connection, id: i64, at: Option<Timestamp>) -> rusqlite::Result<bool> {
    let n = conn.execute(
        "UPDATE packages SET archived_at = ?2 WHERE id = ?1",
        params![id, at.map(secs)],
    )?;
    Ok(n == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn insert_get_and_list() {
        let db = Db::open_in_memory().unwrap();
        let now: Timestamp = "2026-10-18T12:00:00Z".parse().unwrap();
        let mut p = NewPackage::titled("Shoes", now);
        p.courier = Some("CTT".into());
        p.code = Some("482913".into());
        p.code_kind = Some(CodeKind::Pickup);
        p.pickup_point.name = Some("Locker".into());
        let id = db
            .blocking(|c| insert(c, &p, "Added by hand", now))
            .unwrap();
        let got = db.blocking(|c| get(c, id)).unwrap().unwrap();
        assert_eq!(got.title, "Shoes");
        assert_eq!(got.code_kind, Some(CodeKind::Pickup));
        assert_eq!(got.pickup_point.name.as_deref(), Some("Locker"));
        assert_eq!(got.created_at, now);

        let first = db.blocking(|c| events(c, id)).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].status, Some(Status::Announced));

        let mut edit = p.clone();
        edit.title = "Trail shoes".into();
        edit.status = Status::Delivered; // ignored: status has its own path
        assert!(
            db.blocking(|c| update(c, id, &edit, "Edited", now))
                .unwrap()
        );
        let got = db.blocking(|c| get(c, id)).unwrap().unwrap();
        assert_eq!(got.title, "Trail shoes");
        assert_eq!(got.status, Status::Announced);

        let later = now + jiff::SignedDuration::from_hours(1);
        db.blocking(|c| set_status(c, id, Status::Delivered, Source::Manual, later, "Collected"))
            .unwrap();
        let got = db.blocking(|c| get(c, id)).unwrap().unwrap();
        assert_eq!((got.status, got.status_at), (Status::Delivered, later));
        let events = db.blocking(|c| events(c, id)).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].description, "Collected");
        assert_eq!(
            db.blocking(|c| distinct(c, Suggest::Courier)).unwrap(),
            ["CTT"]
        );

        db.blocking(|c| set_archived(c, id, Some(now))).unwrap();
        assert!(db.blocking(|c| list(c, Scope::Active)).unwrap().is_empty());
        assert_eq!(db.blocking(|c| list(c, Scope::Archived)).unwrap().len(), 1);
        assert_eq!(db.blocking(|c| list(c, Scope::All)).unwrap().len(), 1);
    }
}
