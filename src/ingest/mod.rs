//! The message pipeline. Every channel (uploads, shares, the API, the paste
//! box) stores a message and runs the same steps: rules → the package it
//! names, updated or created → or the inbox.

pub mod eml;

use jiff::{SignedDuration, Timestamp};
use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::db::messages::{self, Message, MessageResult, NewMessage, ResultUpdate};
use crate::db::{packages, rules as rule_db};
use crate::model::{Channel, CodeKind, NewPackage, Package, Source, Status};
use crate::rules::engine::{self, CompiledRule, Fields, Input, Outcome};

/// Plain text (shares, the API, the paste box) is capped at this.
pub const MAX_TEXT: usize = 64 * 1024;

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The enabled rules, compiled. A stored rule that no longer compiles
/// (it shouldn't: rules are checked on save) is skipped and logged.
pub fn load_rules(conn: &Connection) -> rusqlite::Result<Vec<CompiledRule>> {
    Ok(rule_db::list(conn)?
        .into_iter()
        .filter(|r| r.enabled)
        .filter_map(|r| match engine::compile(&r.id, r.version, r.def) {
            Ok(c) => Some(c),
            Err(errors) => {
                tracing::error!(rule = %r.id, ?errors, "stored rule does not compile; skipped");
                None
            }
        })
        .collect())
}

/// What became of a message, for the person who sent it in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Processed {
    pub message_id: i64,
    pub result: MessageResult,
    pub rule_name: Option<String>,
    pub error: Option<String>,
    pub fields: Fields,
    pub package_id: Option<i64>,
    pub created: bool,
    /// An earlier upload of the same email, which was not processed again.
    pub duplicate_of: Option<i64>,
}

impl Processed {
    /// One line for a notification ("CTT RR123456789PT – ready for pickup,
    /// code 482913").
    pub fn summary(&self) -> String {
        if let Some(id) = self.duplicate_of {
            return format!("Already received (message {id})");
        }
        match (self.result, self.package_id) {
            (MessageResult::Matched, Some(_)) => {
                let f = &self.fields;
                let mut s = [
                    f.get("courier"),
                    f.get("tracking_number").or(f.get("order_ref")),
                ]
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
                if s.is_empty() {
                    s = self.rule_name.clone().unwrap_or_default();
                }
                if let Some(status) = f.get("status").and_then(|s| Status::parse(s)) {
                    s.push_str(&format!(" – {}", status.label().to_lowercase()));
                }
                if let Some(code) = f.get("code") {
                    s.push_str(&format!(", code {code}"));
                }
                s
            }
            (MessageResult::Matched, None) => {
                "Matched, but which package? Attach it in the inbox".into()
            }
            (MessageResult::Error, _) => format!(
                "A rule matched but failed – in the inbox ({})",
                self.error.as_deref().unwrap_or("")
            ),
            _ => "No rule matched – in the inbox".into(),
        }
    }
}

/// A message as received, before it is stored.
pub struct Incoming {
    pub channel: Channel,
    pub submitted_by: Option<String>,
    pub sender: Option<String>,
    pub subject: Option<String>,
    pub sent_at: Option<Timestamp>,
    pub message_id: Option<String>,
    pub text: String,
    /// The MIME for uploads, the text otherwise.
    pub raw: Vec<u8>,
}

/// Stores a message and runs it through the rules. Uploads already seen
/// (same `Message-ID` or same bytes) are reported, not processed again.
pub fn ingest(
    conn: &mut Connection,
    rules: &[CompiledRule],
    incoming: Incoming,
    retention: SignedDuration,
    now: Timestamp,
) -> rusqlite::Result<Processed> {
    let raw_hash = sha256_hex(&incoming.raw);
    if incoming.channel == Channel::Upload
        && let Some(id) = messages::find_duplicate(conn, incoming.message_id.as_deref(), &raw_hash)?
    {
        let earlier = messages::get(conn, id)?.expect("just found");
        return Ok(Processed {
            message_id: id,
            result: earlier.result,
            rule_name: None,
            error: None,
            fields: earlier.extracted,
            package_id: earlier.package_id,
            created: false,
            duplicate_of: Some(id),
        });
    }
    let tx = conn.transaction()?;
    let id = messages::insert(
        &tx,
        &NewMessage {
            received_at: now,
            sent_at: incoming.sent_at.unwrap_or(now).min(now),
            channel: incoming.channel,
            submitted_by: incoming.submitted_by,
            sender: incoming.sender,
            subject: incoming.subject,
            body_text: incoming.text,
            raw: incoming.raw,
            message_id: incoming.message_id,
            raw_hash,
            purge_at: now + retention,
        },
    )?;
    let processed = process_in(&tx, rules, id, now)?;
    tx.commit()?;
    Ok(processed)
}

/// Runs a stored message through the rules again (after a rule changed).
pub fn process(
    conn: &mut Connection,
    rules: &[CompiledRule],
    id: i64,
    now: Timestamp,
) -> rusqlite::Result<Processed> {
    let tx = conn.transaction()?;
    let processed = process_in(&tx, rules, id, now)?;
    tx.commit()?;
    Ok(processed)
}

fn process_in(
    conn: &Connection,
    rules: &[CompiledRule],
    id: i64,
    now: Timestamp,
) -> rusqlite::Result<Processed> {
    let message = messages::get(conn, id)?.expect("message exists");
    let Some(text) = message.text() else {
        // Purged: nothing left to read.
        return Ok(Processed {
            message_id: id,
            result: message.result,
            rule_name: None,
            error: message.error,
            fields: message.extracted,
            package_id: message.package_id,
            created: false,
            duplicate_of: None,
        });
    };
    let outcome = engine::evaluate(
        rules,
        &Input {
            channel: message.channel,
            sender: message.sender.as_deref(),
            text: &text,
            sent_at: message.sent_at,
        },
    );
    let mut processed = Processed {
        message_id: id,
        result: MessageResult::Unmatched,
        rule_name: None,
        error: None,
        fields: Fields::new(),
        package_id: None,
        created: false,
        duplicate_of: None,
    };
    let rule = outcome.rule().map(|i| &rules[i]);
    match outcome {
        Outcome::Unmatched => {}
        Outcome::Failed { error, fields, .. } => {
            processed.result = MessageResult::Error;
            processed.error = Some(error);
            processed.fields = fields;
        }
        Outcome::Matched { fields, .. } => {
            let rule = rule.expect("matched a rule");
            let (package_id, created) = apply(conn, &rule.def.name, &fields, &message, now)?;
            processed.result = MessageResult::Matched;
            processed.package_id = package_id;
            processed.created = created;
            processed.fields = fields;
        }
    }
    processed.rule_name = rule.map(|r| r.def.name.clone());
    messages::set_result(
        conn,
        id,
        &ResultUpdate {
            result: processed.result,
            rule: rule.map(|r| (r.id.as_str(), r.version)),
            error: processed.error.as_deref(),
            extracted: &processed.fields,
            package_id: processed.package_id,
        },
    )?;
    Ok(processed)
}

fn date_ts(value: &str) -> Option<Timestamp> {
    crate::web::parse_date(value).ok().flatten()
}

/// Writes extracted fields over a package's details. Empty fields never
/// erase what is there.
pub fn merge_fields(p: &mut NewPackage, f: &Fields) {
    let get = |k: &str| f.get(k).filter(|v| !v.is_empty()).cloned();
    if let Some(v) = get("title") {
        p.title = v;
    }
    let set = |slot: &mut Option<String>, k: &str| {
        if let Some(v) = get(k) {
            *slot = Some(v);
        }
    };
    set(&mut p.merchant, "merchant");
    set(&mut p.courier, "courier");
    set(&mut p.tracking_number, "tracking_number");
    set(&mut p.order_ref, "order_ref");
    set(&mut p.code, "code");
    set(&mut p.pickup_point.name, "pickup_point");
    set(&mut p.pickup_point.address, "pickup_address");
    set(&mut p.pickup_point.hours, "pickup_hours");
    if let Some(url) = get("tracking_url").and_then(|u| crate::web::safe_link(&u)) {
        p.tracking_url = Some(url);
    }
    if let Some(kind) = get("code_kind").and_then(|k| CodeKind::parse(&k)) {
        p.code_kind = Some(kind);
    } else if p.code.is_some() && p.code_kind.is_none() {
        p.code_kind = Some(CodeKind::Delivery);
    }
    if let Some(t) = get("code_expires_at").and_then(|d| date_ts(&d)) {
        p.code_expires_at = Some(t);
    }
    if let Some(t) = get("eta").and_then(|d| date_ts(&d)) {
        p.eta_from = Some(t);
        p.eta_to = Some(t);
    }
}

/// Updates the package the fields name, or creates one. A follow-up that
/// names no package returns `None` and waits in the inbox.
fn apply(
    conn: &Connection,
    rule_name: &str,
    fields: &Fields,
    message: &Message,
    now: Timestamp,
) -> rusqlite::Result<(Option<i64>, bool)> {
    let get = |k: &str| fields.get(k).map(String::as_str);
    let (tracking, order) = (get("tracking_number"), get("order_ref"));
    if tracking.is_none() && order.is_none() {
        return Ok((None, false));
    }
    let found = packages::find(conn, get("courier"), tracking, get("merchant"), order)?;
    match found {
        Some(p) => {
            update_package(conn, &p, fields, rule_name, message.sent_at)?;
            Ok((Some(p.id), false))
        }
        None => {
            let mut p = NewPackage::titled(String::new(), message.sent_at);
            merge_fields(&mut p, fields);
            if p.title.is_empty() {
                p.title = default_title(&p);
            }
            p.status = get("status")
                .and_then(Status::parse)
                .unwrap_or(Status::Announced);
            p.status_source = Source::Message;
            let id = packages::insert_in(conn, &p, rule_name, now)?;
            Ok((Some(id), true))
        }
    }
}

pub fn default_title(p: &NewPackage) -> String {
    match (&p.merchant, &p.courier, &p.tracking_number) {
        (Some(m), _, _) => format!("Order from {m}"),
        (None, Some(c), Some(t)) => format!("{c} {t}"),
        (None, None, Some(t)) => t.clone(),
        _ => "Package".into(),
    }
}

/// Applies a message's fields and status to a package, adding one event.
fn update_package(
    conn: &Connection,
    p: &Package,
    fields: &Fields,
    rule_name: &str,
    at: Timestamp,
) -> rusqlite::Result<()> {
    let mut details = p.to_new();
    merge_fields(&mut details, fields);
    packages::replace_details(conn, p.id, &details)?;
    match fields.get("status").and_then(|s| Status::parse(s)) {
        Some(status) if p.accepts(status, at) => {
            packages::set_status_in(conn, p.id, status, Source::Message, at, rule_name)?;
        }
        // Older news, or not forwards: on the timeline only.
        status => {
            packages::insert_event(conn, p.id, at, status, rule_name, Source::Message)?;
        }
    }
    Ok(())
}

/// Links a message to a package by hand, applying what its rule extracted.
pub fn attach(
    conn: &mut Connection,
    message_id: i64,
    package_id: i64,
    by: &str,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    let (Some(message), Some(package)) = (
        messages::get(&tx, message_id)?,
        packages::get(&tx, package_id)?,
    ) else {
        return Ok(false);
    };
    let name = format!("Message attached by {by}");
    if message.extracted.is_empty() {
        packages::insert_event(
            &tx,
            package_id,
            message.sent_at,
            None,
            &name,
            Source::Message,
        )?;
    } else {
        update_package(&tx, &package, &message.extracted, &name, message.sent_at)?;
    }
    messages::attach(&tx, message_id, package_id)?;
    tx.commit()?;
    Ok(true)
}

/// Runs every inbox message through the rules again. Returns how many left
/// the inbox (now linked to a package).
pub fn rerun_inbox(
    conn: &mut Connection,
    rules: &[CompiledRule],
    now: Timestamp,
) -> rusqlite::Result<usize> {
    let mut resolved = 0;
    for m in messages::inbox(conn)? {
        if m.body_text.is_none() {
            continue;
        }
        if process(conn, rules, m.id, now)?.package_id.is_some() {
            resolved += 1;
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::rules::schema::RuleDef;

    fn now() -> Timestamp {
        "2026-10-18T12:00:00Z".parse().unwrap()
    }

    fn rules() -> Vec<CompiledRule> {
        let ready = RuleDef::from_yaml(crate::rules::schema::NEW_RULE).unwrap();
        let transit = RuleDef::from_yaml(
            "slug: ctt-transit\nname: CTT in transit\ncourier: CTT\nmatch: {all: ['(?i)em trânsito']}\n\
             extract: {tracking_number: '(?P<v>[A-Z]{2}\\d{9}PT)', eta: '(?i)entrega prevista (?P<v>\\d{2}/\\d{2})'}\n\
             set: {status: in_transit}\n",
        )
        .unwrap();
        let reminder = RuleDef::from_yaml(
            "slug: ctt-reminder\nname: CTT reminder\nfollowUp: true\nmatch: {all: ['(?i)ainda não levantou']}\n",
        )
        .unwrap();
        [("a", ready), ("b", transit), ("c", reminder)]
            .into_iter()
            .map(|(id, def)| engine::compile(id, 1, def).unwrap())
            .collect()
    }

    fn text(t: &str) -> Incoming {
        Incoming {
            channel: Channel::Share,
            submitted_by: Some("ana".into()),
            sender: None,
            subject: None,
            sent_at: None,
            message_id: None,
            text: t.into(),
            raw: t.as_bytes().to_vec(),
        }
    }

    const DAY: SignedDuration = SignedDuration::from_hours(24);

    #[test]
    fn messages_create_then_update_packages() {
        let db = Db::open_in_memory().unwrap();
        let rules = rules();
        let first = db
            .blocking(|c| {
                ingest(
                    c,
                    &rules,
                    text("Objeto RR123456789PT em trânsito, entrega prevista 21/10."),
                    DAY,
                    now(),
                )
            })
            .unwrap();
        assert_eq!(first.result, MessageResult::Matched);
        assert!(first.created);
        assert_eq!(first.summary(), "CTT RR123456789PT – in transit");
        let id = first.package_id.unwrap();
        let p = db.blocking(|c| packages::get(c, id)).unwrap().unwrap();
        assert_eq!(p.title, "CTT RR123456789PT");
        assert_eq!(p.status, Status::InTransit);
        assert_eq!(p.eta_from, Some("2026-10-21T00:00:00Z".parse().unwrap()));

        let later = now() + SignedDuration::from_hours(5);
        let second = db
            .blocking(|c| {
                ingest(
                    c,
                    &rules,
                    text("CTT: RR123456789PT disponível para levantamento. Código: 482913."),
                    DAY,
                    later,
                )
            })
            .unwrap();
        assert_eq!(second.package_id, Some(id));
        assert!(!second.created);
        assert_eq!(
            second.summary(),
            "CTT RR123456789PT – ready for pickup, code 482913"
        );
        let p = db.blocking(|c| packages::get(c, id)).unwrap().unwrap();
        assert_eq!(p.status, Status::ReadyForPickup);
        assert_eq!(p.code.as_deref(), Some("482913"));
        assert_eq!(
            p.eta_from,
            Some("2026-10-21T00:00:00Z".parse().unwrap()),
            "kept"
        );
        let events = db.blocking(|c| packages::events(c, id)).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].source, Source::Message);

        // An old "in transit" (sent before the pickup notice) is recorded
        // but doesn't move the status back.
        let mut old = text("Objeto RR123456789PT em trânsito.");
        old.sent_at = Some(now() + SignedDuration::from_hours(1));
        db.blocking(|c| ingest(c, &rules, old, DAY, later + SignedDuration::from_hours(1)))
            .unwrap();
        let p = db.blocking(|c| packages::get(c, id)).unwrap().unwrap();
        assert_eq!(p.status, Status::ReadyForPickup);
        assert_eq!(db.blocking(|c| packages::events(c, id)).unwrap().len(), 3);
    }

    #[test]
    fn unmatched_errors_and_follow_ups_wait_in_the_inbox() {
        let db = Db::open_in_memory().unwrap();
        let rules = rules();
        let none = db
            .blocking(|c| ingest(c, &rules, text("Paack: entrega amanhã"), DAY, now()))
            .unwrap();
        assert_eq!(none.result, MessageResult::Unmatched);
        assert_eq!(none.summary(), "No rule matched – in the inbox");
        let failed = db
            .blocking(|c| {
                ingest(
                    c,
                    &rules,
                    text("disponível para levantamento, sem número"),
                    DAY,
                    now(),
                )
            })
            .unwrap();
        assert_eq!(failed.result, MessageResult::Error);
        let follow = db
            .blocking(|c| {
                ingest(
                    c,
                    &rules,
                    text("Ainda não levantou a sua encomenda."),
                    DAY,
                    now(),
                )
            })
            .unwrap();
        assert_eq!(follow.result, MessageResult::Matched);
        assert_eq!(follow.package_id, None);
        let inbox = db.blocking(|c| messages::inbox(c)).unwrap();
        assert_eq!(inbox.len(), 3);

        // Attaching by hand links it; ignoring removes it.
        let mut p = NewPackage::titled("Shoes", now());
        p.courier = Some("CTT".into());
        let pkg = db
            .blocking(|c| packages::insert(c, &p, "Added", now()))
            .unwrap();
        assert!(
            db.blocking(|c| attach(c, follow.message_id, pkg, "ana"))
                .unwrap()
        );
        assert!(
            db.blocking(|c| messages::ignore(c, none.message_id))
                .unwrap()
        );
        let inbox = db.blocking(|c| messages::inbox(c)).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].id, failed.message_id);
    }

    #[test]
    fn a_new_rule_resolves_the_inbox() {
        let db = Db::open_in_memory().unwrap();
        let m = db
            .blocking(|c| {
                ingest(
                    c,
                    &[],
                    text("Objeto RR123456789PT em trânsito."),
                    DAY,
                    now(),
                )
            })
            .unwrap();
        assert_eq!(m.result, MessageResult::Unmatched);
        let rules = rules();
        assert_eq!(db.blocking(|c| rerun_inbox(c, &rules, now())).unwrap(), 1);
        assert!(db.blocking(|c| messages::inbox(c)).unwrap().is_empty());
    }

    #[test]
    fn duplicate_uploads_are_not_processed_again() {
        let db = Db::open_in_memory().unwrap();
        let email = || Incoming {
            channel: Channel::Upload,
            message_id: Some("<a@b>".into()),
            ..text("x")
        };
        let first = db
            .blocking(|c| ingest(c, &[], email(), DAY, now()))
            .unwrap();
        assert_eq!(first.duplicate_of, None);
        let again = db
            .blocking(|c| ingest(c, &[], email(), DAY, now()))
            .unwrap();
        assert_eq!(again.duplicate_of, Some(first.message_id));
        assert_eq!(db.blocking(|c| messages::inbox(c)).unwrap().len(), 1);
        // Shared texts are never duplicates: the same SMS can come twice.
        db.blocking(|c| ingest(c, &[], text("x"), DAY, now()))
            .unwrap();
        db.blocking(|c| ingest(c, &[], text("x"), DAY, now()))
            .unwrap();
        assert_eq!(db.blocking(|c| messages::inbox(c)).unwrap().len(), 3);
    }

    #[test]
    fn purged_messages_keep_their_result() {
        let db = Db::open_in_memory().unwrap();
        let m = db
            .blocking(|c| ingest(c, &[], text("Paack: entrega amanhã"), DAY, now()))
            .unwrap();
        assert_eq!(db.blocking(|c| messages::purge(c, now() + DAY)).unwrap(), 1);
        let stored = db
            .blocking(|c| messages::get(c, m.message_id))
            .unwrap()
            .unwrap();
        assert_eq!(stored.body_text, None);
        assert_eq!(stored.result, MessageResult::Unmatched);
        let rules = rules();
        assert_eq!(db.blocking(|c| rerun_inbox(c, &rules, now())).unwrap(), 0);
    }
}
