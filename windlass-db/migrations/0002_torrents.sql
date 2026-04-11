CREATE TABLE IF NOT EXISTS torrents (
    hash              TEXT PRIMARY KEY,
    book_id           INTEGER REFERENCES books(id),
    mam_id            INTEGER,
    name              TEXT NOT NULL,
    state             TEXT NOT NULL,
    seeding_time_secs INTEGER NOT NULL DEFAULT 0,
    downloaded_bytes  INTEGER NOT NULL DEFAULT 0,
    seen_at           TEXT NOT NULL,
    added_at          TEXT NOT NULL DEFAULT (datetime('now'))
);
