ALTER TABLE torrents
    ADD CONSTRAINT torrents_state_valid CHECK (state IN (
        'downloading',
        'uploading',
        'forcedUP',
        'pausedDL',
        'pausedUP',
        'stalledDL',
        'stalledUP',
        'checking',
        'error',
        'other'
    )),
    ADD CONSTRAINT torrents_seeding_time_nonneg CHECK (seeding_time_secs >= 0),
    ADD CONSTRAINT torrents_downloaded_bytes_nonneg CHECK (downloaded_bytes >= 0);

CREATE UNIQUE INDEX download_queue_one_active_mam_idx
    ON download_queue(mam_id)
    WHERE status IN ('pending', 'downloading', 'seeding');
