-- status values: pending | downloading | seeding | satisfied | failed | blacklisted
CREATE TABLE IF NOT EXISTS download_queue (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    book_id    INTEGER REFERENCES books(id),
    mam_id     INTEGER NOT NULL,
    status     TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
