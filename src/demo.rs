//! Sample packages for `--demo` mode. Every name, number and code is made up.

use jiff::{SignedDuration, Timestamp, ToSpan};
use rusqlite::Connection;

use crate::db::packages;
use crate::model::{CodeKind, NewPackage, PickupPoint, Source, Status};

fn hours(n: i64) -> SignedDuration {
    SignedDuration::from_hours(n)
}

/// Midnight UTC `n` days from `now`'s date, as ETAs are dates.
fn day(now: Timestamp, n: i64) -> Timestamp {
    let date = now.to_zoned(jiff::tz::TimeZone::UTC).date();
    date.checked_add(n.days())
        .and_then(|d| d.to_zoned(jiff::tz::TimeZone::UTC))
        .map(|z| z.timestamp())
        .unwrap_or(now)
}

struct Sample {
    package: NewPackage,
    /// (hours before now, status, description), oldest first. The last one
    /// is the package's current status.
    events: Vec<(i64, Status, &'static str)>,
    archived_hours_ago: Option<i64>,
}

fn samples(now: Timestamp) -> Vec<Sample> {
    let base = |title: &str| NewPackage::titled(title, now);
    vec![
        Sample {
            package: NewPackage {
                merchant: Some("Decathlon".into()),
                recipient: Some("Ana".into()),
                courier: Some("CTT".into()),
                tracking_number: Some("RR123456789PT".into()),
                order_ref: Some("DEC-58213".into()),
                code: Some("482913".into()),
                code_kind: Some(CodeKind::Pickup),
                code_expires_at: Some(day(now, 6)),
                pickup_point: PickupPoint {
                    name: Some("CTT Locker – Mercado de Benfica".into()),
                    address: Some("Rua Cláudio Nunes 12, 1500-000 Lisboa".into()),
                    hours: Some("24h".into()),
                },
                ..base("Trail running shoes")
            },
            events: vec![
                (72, Status::Announced, "Order confirmed"),
                (50, Status::InTransit, "Accepted by CTT"),
                (20, Status::ReadyForPickup, "Ready for pickup at the locker"),
            ],
            archived_hours_ago: None,
        },
        Sample {
            package: NewPackage {
                merchant: Some("Flor da Selva".into()),
                recipient: Some("David".into()),
                courier: Some("Paack".into()),
                tracking_number: Some("PCK8F3K2Q".into()),
                code: Some("7315".into()),
                code_kind: Some(CodeKind::Delivery),
                eta_from: Some(day(now, 0)),
                eta_to: Some(day(now, 0)),
                ..base("Coffee beans, 1 kg")
            },
            events: vec![
                (30, Status::Announced, "Label created"),
                (3, Status::OutForDelivery, "Out for delivery, 14:00–16:00"),
            ],
            archived_hours_ago: None,
        },
        Sample {
            package: NewPackage {
                merchant: Some("AliExpress".into()),
                courier: Some("CTT".into()),
                tracking_number: Some("LP00412345678901".into()),
                order_ref: Some("8123004417".into()),
                eta_from: Some(day(now, 3)),
                eta_to: Some(day(now, 6)),
                ..base("Phone case")
            },
            events: vec![
                (240, Status::Announced, "Order confirmed"),
                (120, Status::InTransit, "Arrived in Portugal"),
            ],
            archived_hours_ago: None,
        },
        Sample {
            package: NewPackage {
                merchant: Some("Wook".into()),
                recipient: Some("Ana".into()),
                courier: Some("CTT Expresso".into()),
                order_ref: Some("W-2026-339120".into()),
                eta_from: Some(day(now, 5)),
                eta_to: Some(day(now, 7)),
                ..base("Books (3)")
            },
            events: vec![(10, Status::Announced, "Order confirmed")],
            archived_hours_ago: None,
        },
        Sample {
            package: NewPackage {
                merchant: Some("Worten".into()),
                courier: Some("GLS".into()),
                tracking_number: Some("ZX81Q7700PT".into()),
                notes: Some("Nobody was home on Thursday. Call to reschedule.".into()),
                ..base("Kitchen scale")
            },
            events: vec![
                (150, Status::Announced, "Order confirmed"),
                (100, Status::InTransit, "In transit"),
                (
                    30,
                    Status::Exception,
                    "Delivery attempt failed: recipient absent",
                ),
            ],
            archived_hours_ago: None,
        },
        Sample {
            package: NewPackage {
                merchant: Some("Amazon".into()),
                recipient: Some("David".into()),
                courier: Some("Paack".into()),
                tracking_number: Some("PCK2M9X0L".into()),
                ..base("Printer ink")
            },
            events: vec![
                (90, Status::Announced, "Order confirmed"),
                (60, Status::OutForDelivery, "Out for delivery"),
                (55, Status::Delivered, "Delivered"),
            ],
            archived_hours_ago: None,
        },
        Sample {
            package: NewPackage {
                merchant: Some("IKEA".into()),
                courier: Some("CTT Expresso".into()),
                tracking_number: Some("EA987654321PT".into()),
                ..base("Desk lamp")
            },
            events: vec![
                (400, Status::Announced, "Order confirmed"),
                (300, Status::Delivered, "Delivered"),
            ],
            archived_hours_ago: Some(130),
        },
    ]
}

/// Fills an empty database with the samples, dated relative to `now`.
pub fn load(conn: &mut Connection, now: Timestamp) -> rusqlite::Result<()> {
    for sample in samples(now) {
        let mut events = sample.events.into_iter();
        let (first_ago, first_status, first_desc) = events.next().expect("samples have events");
        let mut p = sample.package;
        p.status = first_status;
        p.status_source = Source::Message;
        p.status_at = now - hours(first_ago);
        let created = p.status_at;
        let id = packages::insert(conn, &p, first_desc, created)?;
        let mut last = (first_ago, first_status);
        for (ago, status, desc) in events {
            packages::insert_event(
                conn,
                id,
                now - hours(ago),
                Some(status),
                desc,
                Source::Message,
            )?;
            last = (ago, status);
        }
        conn.execute(
            "UPDATE packages SET status = ?2, status_at = ?3 WHERE id = ?1",
            rusqlite::params![id, last.1.as_str(), (now - hours(last.0)).as_second()],
        )?;
        if let Some(ago) = sample.archived_hours_ago {
            packages::set_archived(conn, id, Some(now - hours(ago)))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::db::packages::Scope;

    #[test]
    fn samples_load() {
        let db = Db::open_in_memory().unwrap();
        let now: Timestamp = "2026-10-18T12:00:00Z".parse().unwrap();
        db.blocking(|c| load(c, now)).unwrap();
        let active = db.blocking(|c| packages::list(c, Scope::Active)).unwrap();
        assert_eq!(active.len(), 6);
        let shoes = &active[0];
        assert_eq!(shoes.status, Status::ReadyForPickup);
        assert_eq!(shoes.status_at, now - hours(20));
        let events = db.blocking(|c| packages::events(c, shoes.id)).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].description, "Ready for pickup at the locker");
    }
}
