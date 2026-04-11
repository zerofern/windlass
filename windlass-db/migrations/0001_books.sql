CREATE TABLE IF NOT EXISTS books (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    mam_id     INTEGER UNIQUE,
    title      TEXT,
    author     TEXT,
    status     TEXT NOT NULL DEFAULT 'pending_metadata',
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
