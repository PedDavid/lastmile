-- Times are Unix seconds (UTC).
CREATE TABLE packages (
    id               INTEGER PRIMARY KEY,
    title            TEXT    NOT NULL,
    merchant         TEXT,
    recipient        TEXT,
    courier          TEXT,
    tracking_number  TEXT,
    order_ref        TEXT,
    tracking_url     TEXT,
    status           TEXT    NOT NULL,
    status_source    TEXT    NOT NULL,
    status_at        INTEGER NOT NULL,
    eta_from         INTEGER,
    eta_to           INTEGER,
    code             TEXT,
    code_kind        TEXT,
    code_expires_at  INTEGER,
    pickup_name      TEXT,
    pickup_address   TEXT,
    pickup_hours     TEXT,
    notes            TEXT,
    created_at       INTEGER NOT NULL,
    archived_at      INTEGER
);

CREATE INDEX packages_tracking ON packages (courier COLLATE NOCASE, tracking_number);
CREATE INDEX packages_order ON packages (merchant COLLATE NOCASE, order_ref);

CREATE TABLE events (
    id           INTEGER PRIMARY KEY,
    package_id   INTEGER NOT NULL REFERENCES packages (id) ON DELETE CASCADE,
    at           INTEGER NOT NULL,
    status       TEXT,
    description  TEXT    NOT NULL,
    source       TEXT    NOT NULL,
    raw_ref      TEXT
);

CREATE INDEX events_package ON events (package_id, at);
