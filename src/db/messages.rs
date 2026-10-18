//! Queries on `messages`.

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{opt_ts, parse_col, secs, ts};
use crate::model::Channel;
use crate::rules::engine::Fields;

/// What happened to a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageResult {
    Matched,
    Unmatched,
    Ignored,
    Error,
}

impl MessageResult {
    pub const ALL: [MessageResult; 4] = [
        MessageResult::Matched,
        MessageResult::Unmatched,
        MessageResult::Ignored,
        MessageResult::Error,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            MessageResult::Matched => "matched",
            MessageResult::Unmatched => "unmatched",
            MessageResult::Ignored => "ignored",
            MessageResult::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<MessageResult> {
        MessageResult::ALL.into_iter().find(|r| r.as_str() == s)
    }
}

#[derive(Clone, Debug)]
pub struct Message {
    pub id: i64,
    pub received_at: Timestamp,
    pub sent_at: Timestamp,
    pub channel: Channel,
    pub submitted_by: Option<String>,
    pub sender: Option<String>,
    pub subject: Option<String>,
    /// `None` once purged.
    pub body_text: Option<String>,
    pub message_id: Option<String>,
    pub rule_id: Option<String>,
    pub rule_version: Option<i64>,
    pub result: MessageResult,
    pub error: Option<String>,
    pub extracted: Fields,
    pub package_id: Option<i64>,
    pub purge_at: Option<Timestamp>,
}

impl Message {
    /// The text rules run on: the subject (emails) and the body.
    pub fn text(&self) -> Option<String> {
        let body = self.body_text.as_deref()?;
        Some(match &self.subject {
            Some(s) if !s.is_empty() => format!("{s}\n\n{body}"),
            _ => body.to_string(),
        })
    }
}

/// A message about to be stored.
pub struct NewMessage {
    pub received_at: Timestamp,
    pub sent_at: Timestamp,
    pub channel: Channel,
    pub submitted_by: Option<String>,
    pub sender: Option<String>,
    pub subject: Option<String>,
    pub body_text: String,
    pub raw: Vec<u8>,
    pub message_id: Option<String>,
    pub raw_hash: String,
    pub purge_at: Timestamp,
}

const COLUMNS: &str = "id, received_at, sent_at, channel, submitted_by, sender, subject, body_text, \
    message_id, rule_id, rule_version, result, error, extracted, package_id, purge_at";

fn message(row: &Row) -> rusqlite::Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        received_at: ts(row.get(1)?)?,
        sent_at: ts(row.get(2)?)?,
        channel: parse_col(row.get(3)?, Channel::parse)?,
        submitted_by: row.get(4)?,
        sender: row.get(5)?,
        subject: row.get(6)?,
        body_text: row.get(7)?,
        message_id: row.get(8)?,
        rule_id: row.get(9)?,
        rule_version: row.get(10)?,
        result: parse_col(row.get(11)?, MessageResult::parse)?,
        error: row.get(12)?,
        extracted: row
            .get::<_, Option<String>>(13)?
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default(),
        package_id: row.get(14)?,
        purge_at: opt_ts(row.get(15)?)?,
    })
}

/// Stores a message as unmatched; [`set_result`] records what the rules
/// made of it.
pub fn insert(conn: &Connection, m: &NewMessage) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO messages (received_at, sent_at, channel, submitted_by, sender, subject, \
         body_text, raw, message_id, raw_hash, result, purge_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'unmatched', ?11)",
        params![
            secs(m.received_at),
            secs(m.sent_at),
            m.channel.as_str(),
            m.submitted_by,
            m.sender,
            m.subject,
            m.body_text,
            m.raw,
            m.message_id,
            m.raw_hash,
            secs(m.purge_at),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub struct ResultUpdate<'a> {
    pub result: MessageResult,
    pub rule: Option<(&'a str, i64)>,
    pub error: Option<&'a str>,
    pub extracted: &'a Fields,
    pub package_id: Option<i64>,
}

pub fn set_result(conn: &Connection, id: i64, u: &ResultUpdate) -> rusqlite::Result<()> {
    let extracted = (!u.extracted.is_empty())
        .then(|| serde_json::to_string(u.extracted).expect("fields serialise"));
    conn.execute(
        "UPDATE messages SET result = ?2, rule_id = ?3, rule_version = ?4, error = ?5, \
         extracted = ?6, package_id = ?7 WHERE id = ?1",
        params![
            id,
            u.result.as_str(),
            u.rule.map(|r| r.0),
            u.rule.map(|r| r.1),
            u.error,
            extracted,
            u.package_id,
        ],
    )?;
    Ok(())
}

pub fn attach(conn: &Connection, id: i64, package_id: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE messages SET package_id = ?2 WHERE id = ?1",
        params![id, package_id],
    )? == 1)
}

pub fn ignore(conn: &Connection, id: i64) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE messages SET result = 'ignored' WHERE id = ?1 AND package_id IS NULL",
        [id],
    )? == 1)
}

pub fn get(conn: &Connection, id: i64) -> rusqlite::Result<Option<Message>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM messages WHERE id = ?1"),
        [id],
        message,
    )
    .optional()
}

/// Messages waiting for a person: not linked to a package and not ignored.
/// Newest first.
pub fn inbox(conn: &Connection) -> rusqlite::Result<Vec<Message>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM messages WHERE package_id IS NULL AND result != 'ignored' \
         ORDER BY received_at DESC, id DESC"
    ))?;
    stmt.query_map([], message)?.collect()
}

/// The messages linked to a package, oldest first.
pub fn for_package(conn: &Connection, package_id: i64) -> rusqlite::Result<Vec<Message>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM messages WHERE package_id = ?1 ORDER BY sent_at, id"
    ))?;
    stmt.query_map([package_id], message)?.collect()
}

/// An earlier upload with the same `Message-ID` or the same bytes.
pub fn find_duplicate(
    conn: &Connection,
    message_id: Option<&str>,
    raw_hash: &str,
) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT id FROM messages WHERE raw_hash = ?1 OR (?2 IS NOT NULL AND message_id = ?2) \
         ORDER BY id LIMIT 1",
        params![raw_hash, message_id],
        |r| r.get(0),
    )
    .optional()
}

/// Per-rule hit counts and the last time each matched.
pub fn rule_hits(conn: &Connection) -> rusqlite::Result<Vec<(String, i64, Timestamp)>> {
    let mut stmt = conn.prepare(
        "SELECT rule_id, COUNT(*), MAX(received_at) FROM messages \
         WHERE rule_id IS NOT NULL AND result = 'matched' GROUP BY rule_id",
    )?;
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, ts(r.get(2)?)?)))?
        .collect()
}

/// Inbox counts by result, for the metrics.
pub fn inbox_counts(conn: &Connection) -> rusqlite::Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT result, COUNT(*) FROM messages WHERE package_id IS NULL AND result != 'ignored' \
         GROUP BY result",
    )?;
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect()
}

/// Empties the text of messages past their retention. Returns how many.
pub fn purge(conn: &Connection, now: Timestamp) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE messages SET body_text = NULL, raw = NULL \
         WHERE purge_at IS NOT NULL AND purge_at <= ?1 AND (body_text IS NOT NULL OR raw IS NOT NULL)",
        [secs(now)],
    )
}
