//! Sample packages for `--demo` mode. Every name, number and code is made up.

use jiff::{SignedDuration, Timestamp, ToSpan};
use rusqlite::Connection;

use crate::db::messages::{self, MessageResult, NewMessage, ResultUpdate};
use crate::db::packages;
use crate::db::rules::{self as rule_db, Origin};
use crate::db::settings;
use crate::ingest::{self, Incoming};
use crate::model::{Channel, CodeKind, NewPackage, PickupPoint, Source, Status};
use crate::redact::Household;
use crate::rules::engine::{self, Input, Outcome};
use crate::rules::schema::RuleDef;

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

/// The example rules, also used by the tests (`tests/fixtures/rules/`).
pub const RULES: [&str; 4] = [
    include_str!("../tests/fixtures/rules/ctt-ready-for-pickup.yaml"),
    include_str!("../tests/fixtures/rules/ctt-in-transit.yaml"),
    include_str!("../tests/fixtures/rules/paack-out-for-delivery.yaml"),
    include_str!("../tests/fixtures/rules/ctt-pickup-reminder.yaml"),
];

/// Stable ids for the example rules, so links survive restarts.
pub fn rule_id(slug: &str) -> String {
    const NAMESPACE: uuid::Uuid = uuid::Uuid::from_u128(0x6c61_7374_6d69_6c65_2d64_656d_6f2d_7275);
    uuid::Uuid::new_v5(&NAMESPACE, slug.as_bytes()).to_string()
}

/// Fills an empty database with the samples, dated relative to `now`.
pub fn load(conn: &mut Connection, now: Timestamp) -> rusqlite::Result<()> {
    load_packages(conn, now)?;
    load_rules_and_messages(conn, now)
}

/// The made-up household of the demo: what its messages mention.
pub fn household() -> Household {
    Household {
        names: vec!["Ana Ferreira".into(), "Ana".into(), "David".into()],
        addresses: vec!["Avenida da Liberdade 10".into()],
        postcodes: vec!["1250-096".into()],
        phones: vec!["912345678".into()],
        emails: vec!["ana@example.com".into()],
    }
}

fn load_rules_and_messages(conn: &mut Connection, now: Timestamp) -> rusqlite::Result<()> {
    settings::set_household(conn, &household())?;
    let samples: [(&str, Channel, &str); 3] = [
        (
            "ctt-ready-for-pickup",
            Channel::Share,
            include_str!("../tests/fixtures/messages/ctt-ready-for-pickup.txt"),
        ),
        (
            "paack-out-for-delivery",
            Channel::Share,
            include_str!("../tests/fixtures/messages/paack-out-for-delivery.txt"),
        ),
        (
            "ctt-pickup-reminder",
            Channel::Share,
            include_str!("../tests/fixtures/messages/ctt-pickup-reminder.txt"),
        ),
    ];
    let mut compiled = Vec::new();
    for source in RULES {
        let def = RuleDef::from_yaml(source).expect("example rules parse");
        let id = rule_id(&def.slug);
        rule_db::save(
            conn,
            &id,
            source,
            &def,
            Origin::Local,
            true,
            "demo",
            now - hours(240),
        )?;
        compiled.push(engine::compile(&id, 1, def).expect("example rules compile"));
    }
    for (slug, channel, text) in samples {
        let id = rule_id(slug);
        let rule = compiled.iter().find(|r| r.id == id).expect("sample's rule");
        let input = Input {
            channel,
            sender: None,
            text,
            sent_at: now,
        };
        if let Outcome::Matched { fields, .. } =
            engine::evaluate(std::slice::from_ref(rule), &input)
        {
            rule_db::add_sample(conn, &id, None, channel, None, text.trim(), &fields, now)?;
        }
    }

    // The pickup notice of the shoes, already applied (its event is in the
    // timeline), linked so the package shows where it came from.
    let text = include_str!("../tests/fixtures/messages/ctt-ready-for-pickup.txt").trim();
    let at = now - hours(20);
    let msg = messages::insert(
        conn,
        &NewMessage {
            received_at: at,
            sent_at: at,
            channel: Channel::Share,
            submitted_by: Some("Ana".into()),
            sender: None,
            subject: None,
            body_text: text.into(),
            raw: text.as_bytes().to_vec(),
            message_id: None,
            raw_hash: ingest::sha256_hex(text.as_bytes()),
            purge_at: at + hours(30 * 24),
        },
    )?;
    let rule = &compiled[0];
    let fields = match engine::evaluate(
        std::slice::from_ref(rule),
        &Input {
            channel: Channel::Share,
            sender: None,
            text,
            sent_at: at,
        },
    ) {
        Outcome::Matched { fields, .. } => fields,
        _ => Default::default(),
    };
    messages::set_result(
        conn,
        msg,
        &ResultUpdate {
            result: MessageResult::Matched,
            rule: Some((&rule.id, 1)),
            error: None,
            extracted: &fields,
            package_id: Some(1),
        },
    )?;

    // Messages waiting in the inbox: one no rule reads, an email from a
    // shop, a pickup notice without a tracking number, and a reminder that
    // doesn't say which package it is about.
    let inbox: [(i64, Channel, &str); 5] = [
        (
            1,
            Channel::Share,
            include_str!("../tests/fixtures/messages/paack-delivery-with-details.txt"),
        ),
        (
            2,
            Channel::Share,
            include_str!("../tests/fixtures/messages/paack-tomorrow.txt"),
        ),
        (
            5,
            Channel::Upload,
            include_str!("../tests/fixtures/messages/wook-order.eml"),
        ),
        (
            9,
            Channel::Paste,
            include_str!("../tests/fixtures/messages/ctt-ready-no-number.txt"),
        ),
        (
            26,
            Channel::Share,
            include_str!("../tests/fixtures/messages/ctt-pickup-reminder.txt"),
        ),
    ];
    for (ago, channel, raw) in inbox.into_iter().rev() {
        let at = now - hours(ago);
        let incoming = if channel == Channel::Upload {
            let email = ingest::eml::parse(raw.as_bytes()).expect("example email parses");
            Incoming {
                channel,
                submitted_by: Some("David".into()),
                sender: email.sender,
                subject: email.subject,
                sent_at: Some(at),
                message_id: email.message_id,
                text: email.text,
                raw: raw.as_bytes().to_vec(),
            }
        } else {
            Incoming {
                channel,
                submitted_by: Some("Ana".into()),
                sender: None,
                subject: None,
                sent_at: None,
                message_id: None,
                text: raw.trim().into(),
                raw: raw.as_bytes().to_vec(),
            }
        };
        ingest::ingest(conn, &compiled, incoming, hours(30 * 24), at)?;
    }
    Ok(())
}

fn load_packages(conn: &mut Connection, now: Timestamp) -> rusqlite::Result<()> {
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

        let rules = db.blocking(|c| rule_db::list(c)).unwrap();
        assert_eq!(rules.len(), 4);
        assert_eq!(db.blocking(|c| rule_db::all_samples(c)).unwrap().len(), 3);
        let inbox = db.blocking(|c| messages::inbox(c)).unwrap();
        let results: Vec<_> = inbox.iter().map(|m| m.result).collect();
        assert_eq!(
            results,
            [
                MessageResult::Unmatched,
                MessageResult::Unmatched,
                MessageResult::Unmatched,
                MessageResult::Error,
                MessageResult::Matched
            ]
        );
        // The demo messages touch no package.
        assert_eq!(db.blocking(|c| packages::events(c, 1)).unwrap().len(), 3);
        assert_eq!(
            db.blocking(|c| messages::for_package(c, 1)).unwrap().len(),
            1
        );
    }
}
