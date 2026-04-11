CREATE TABLE IF NOT EXISTS events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    source     TEXT NOT NULL,
    action     TEXT NOT NULL,
    book_id    INTEGER REFERENCES books(id),
    detail     TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
