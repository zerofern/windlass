CREATE TABLE IF NOT EXISTS books (
    id         BIGSERIAL PRIMARY KEY,
    mam_id     BIGINT UNIQUE,
    title      TEXT,
    author     TEXT,
    status     TEXT NOT NULL DEFAULT 'pending_metadata',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT books_status_valid CHECK (
        status IN ('pending_metadata', 'queued', 'downloading', 'complete', 'failed')
    )
);

CREATE TABLE IF NOT EXISTS torrents (
    hash              TEXT PRIMARY KEY,
    book_id           BIGINT REFERENCES books(id),
    mam_id            BIGINT,
    name              TEXT NOT NULL,
    state             TEXT NOT NULL,
    seeding_time_secs BIGINT NOT NULL DEFAULT 0,
    downloaded_bytes  BIGINT NOT NULL DEFAULT 0,
    seen_at           TIMESTAMPTZ NOT NULL,
    added_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS torrents_book_id_idx ON torrents(book_id);
CREATE INDEX IF NOT EXISTS torrents_mam_id_idx ON torrents(mam_id);
CREATE INDEX IF NOT EXISTS torrents_added_at_idx ON torrents(added_at DESC);

CREATE TABLE IF NOT EXISTS download_queue (
    id         BIGSERIAL PRIMARY KEY,
    book_id    BIGINT REFERENCES books(id),
    mam_id     BIGINT NOT NULL,
    status     TEXT NOT NULL DEFAULT 'pending',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT download_queue_status_valid CHECK (
        status IN ('pending', 'downloading', 'seeding', 'satisfied', 'failed', 'blacklisted')
    )
);

CREATE INDEX IF NOT EXISTS download_queue_book_id_idx ON download_queue(book_id);
CREATE INDEX IF NOT EXISTS download_queue_mam_id_idx ON download_queue(mam_id);
CREATE INDEX IF NOT EXISTS download_queue_created_at_idx ON download_queue(created_at DESC);

CREATE TABLE IF NOT EXISTS activity_log (
    id         BIGSERIAL PRIMARY KEY,
    source     TEXT NOT NULL,
    action     TEXT NOT NULL,
    book_id    BIGINT REFERENCES books(id),
    detail     TEXT,
    metadata   JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS activity_log_book_id_idx ON activity_log(book_id);
CREATE INDEX IF NOT EXISTS activity_log_created_at_idx ON activity_log(created_at DESC);

CREATE TABLE IF NOT EXISTS alerts (
    id         BIGSERIAL PRIMARY KEY,
    priority   TEXT NOT NULL,
    title      TEXT NOT NULL,
    body       TEXT NOT NULL,
    read       BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT alerts_priority_valid CHECK (
        priority IN ('info', 'warning', 'critical')
    )
);

CREATE INDEX IF NOT EXISTS alerts_created_at_idx ON alerts(created_at DESC);
CREATE INDEX IF NOT EXISTS alerts_unread_idx ON alerts(read) WHERE read = false;

CREATE TABLE IF NOT EXISTS system_snapshots (
    id         BIGSERIAL PRIMARY KEY,
    state      JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS system_snapshots_created_at_idx ON system_snapshots(created_at DESC);
