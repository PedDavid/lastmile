-- Messages: everything that came in (uploads, shares, the API, the paste
-- box). The text is personal data: body_text and raw are emptied at
-- purge_at, the rest is kept for the inbox history and duplicates.
CREATE TABLE messages (
    id            INTEGER PRIMARY KEY,
    received_at   INTEGER NOT NULL,
    -- When the message says it was sent (an email's Date), else received_at.
    sent_at       INTEGER NOT NULL,
    channel       TEXT    NOT NULL,
    submitted_by  TEXT,
    sender        TEXT,
    subject       TEXT,
    body_text     TEXT,
    raw           BLOB,
    message_id    TEXT,
    raw_hash      TEXT    NOT NULL,
    rule_id       TEXT,
    rule_version  INTEGER,
    result        TEXT    NOT NULL,
    error         TEXT,
    extracted     TEXT,
    package_id    INTEGER REFERENCES packages (id) ON DELETE SET NULL,
    purge_at      INTEGER
);

CREATE INDEX messages_package ON messages (package_id);
CREATE INDEX messages_duplicates ON messages (raw_hash);
CREATE INDEX messages_purge ON messages (purge_at);

-- Rules: the current version of each, plus every saved version.
CREATE TABLE rules (
    id          TEXT    PRIMARY KEY,
    version     INTEGER NOT NULL,
    name        TEXT    NOT NULL,
    enabled     INTEGER NOT NULL,
    source      TEXT    NOT NULL,
    definition  TEXT    NOT NULL,
    origin      TEXT    NOT NULL,
    created_by  TEXT,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE TABLE rule_versions (
    rule_id     TEXT    NOT NULL REFERENCES rules (id) ON DELETE CASCADE,
    version     INTEGER NOT NULL,
    source      TEXT    NOT NULL,
    created_by  TEXT,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (rule_id, version)
);

-- Regression samples, re-run whenever a rule is saved. They keep their own
-- copy of the text, so purging messages doesn't lose them.
CREATE TABLE rule_samples (
    id          INTEGER PRIMARY KEY,
    rule_id     TEXT    NOT NULL REFERENCES rules (id) ON DELETE CASCADE,
    message_id  INTEGER REFERENCES messages (id) ON DELETE SET NULL,
    channel     TEXT    NOT NULL,
    sender      TEXT,
    text        TEXT    NOT NULL,
    expected    TEXT    NOT NULL,
    created_at  INTEGER NOT NULL
);

-- Ingest tokens (the iOS Shortcut). Only a SHA-256 of each is stored.
CREATE TABLE tokens (
    id            INTEGER PRIMARY KEY,
    user_sub      TEXT    NOT NULL,
    user_name     TEXT    NOT NULL,
    name          TEXT    NOT NULL,
    hash          TEXT    NOT NULL UNIQUE,
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER,
    revoked_at    INTEGER
);
