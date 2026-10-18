//! Ingest tokens: random secrets shown once, stored as SHA-256 hashes.

use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

use super::{opt_ts, secs, ts};

/// Tokens start with this, so they are recognisable in a Shortcut or a leak scan.
pub const PREFIX: &str = "lm_";

#[derive(Clone, Debug)]
pub struct Token {
    pub id: i64,
    pub user_sub: String,
    pub user_name: String,
    pub name: String,
    pub created_at: Timestamp,
    pub last_used_at: Option<Timestamp>,
}

pub fn hash(token: &str) -> String {
    Sha256::digest(token.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A new random token (32 bytes, base64url).
pub fn generate() -> String {
    format!("{PREFIX}{}", crate::web::auth::random_token())
}

pub fn create(
    conn: &Connection,
    user_sub: &str,
    user_name: &str,
    name: &str,
    token: &str,
    now: Timestamp,
) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO tokens (user_sub, user_name, name, hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![user_sub, user_name, name, hash(token), secs(now)],
    )?;
    Ok(conn.last_insert_rowid())
}

/// The user's tokens that are not revoked, newest first.
pub fn list(conn: &Connection, user_sub: &str) -> rusqlite::Result<Vec<Token>> {
    let mut stmt = conn.prepare(
        "SELECT id, user_sub, user_name, name, created_at, last_used_at FROM tokens \
         WHERE user_sub = ?1 AND revoked_at IS NULL ORDER BY created_at DESC, id DESC",
    )?;
    stmt.query_map([user_sub], |r| {
        Ok(Token {
            id: r.get(0)?,
            user_sub: r.get(1)?,
            user_name: r.get(2)?,
            name: r.get(3)?,
            created_at: ts(r.get(4)?)?,
            last_used_at: opt_ts(r.get(5)?)?,
        })
    })?
    .collect()
}

/// Revokes one of the user's tokens.
pub fn revoke(
    conn: &Connection,
    user_sub: &str,
    id: i64,
    now: Timestamp,
) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "UPDATE tokens SET revoked_at = ?3 WHERE id = ?1 AND user_sub = ?2 AND revoked_at IS NULL",
        params![id, user_sub, secs(now)],
    )? == 1)
}

/// The live token with this secret, marking it used.
pub fn authenticate(
    conn: &Connection,
    token: &str,
    now: Timestamp,
) -> rusqlite::Result<Option<Token>> {
    if !token.starts_with(PREFIX) {
        return Ok(None);
    }
    let found = conn
        .query_row(
            "SELECT id, user_sub, user_name, name, created_at, last_used_at FROM tokens \
             WHERE hash = ?1 AND revoked_at IS NULL",
            [hash(token)],
            |r| {
                Ok(Token {
                    id: r.get(0)?,
                    user_sub: r.get(1)?,
                    user_name: r.get(2)?,
                    name: r.get(3)?,
                    created_at: ts(r.get(4)?)?,
                    last_used_at: opt_ts(r.get(5)?)?,
                })
            },
        )
        .optional()?;
    if let Some(t) = &found {
        conn.execute(
            "UPDATE tokens SET last_used_at = ?2 WHERE id = ?1",
            params![t.id, secs(now)],
        )?;
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    #[test]
    fn tokens_authenticate_until_revoked() {
        let db = Db::open_in_memory().unwrap();
        let now: Timestamp = "2026-10-18T12:00:00Z".parse().unwrap();
        let secret = generate();
        assert!(secret.starts_with("lm_") && secret.len() > 40);
        let id = db
            .blocking(|c| create(c, "u1", "ana", "iPhone", &secret, now))
            .unwrap();
        let stored: String = db
            .blocking(|c| c.query_row("SELECT hash FROM tokens", [], |r| r.get(0)))
            .unwrap();
        assert_ne!(stored, secret, "only the hash is stored");

        let t = db
            .blocking(|c| authenticate(c, &secret, now))
            .unwrap()
            .unwrap();
        assert_eq!((t.user_name.as_str(), t.name.as_str()), ("ana", "iPhone"));
        assert_eq!(
            db.blocking(|c| list(c, "u1")).unwrap()[0].last_used_at,
            Some(now)
        );
        assert!(
            db.blocking(|c| authenticate(c, "lm_nope", now))
                .unwrap()
                .is_none()
        );

        // Only its owner can revoke it.
        assert!(!db.blocking(|c| revoke(c, "u2", id, now)).unwrap());
        assert!(db.blocking(|c| revoke(c, "u1", id, now)).unwrap());
        assert!(
            db.blocking(|c| authenticate(c, &secret, now))
                .unwrap()
                .is_none()
        );
        assert!(db.blocking(|c| list(c, "u1")).unwrap().is_empty());
    }
}
