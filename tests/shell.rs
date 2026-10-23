//! Integration tests using real `SQLite` files, console streams, and shell workers.

use std::{error::Error, path::Path, time::Duration};

use rusqlite::Connection;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use windlass::{
    core::{
        MamPreferencePersistence, MamPreferenceState, MamPreferenceUpdate, MamRequestMode,
        StartupMode, State,
    },
    shell::console,
};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

async fn run(path: &Path, mode: StartupMode, input: &[u8]) -> TestResult<(State, String)> {
    let mut output = Vec::new();
    let status = console::run(path, mode, input, &mut output).await?;
    Ok((status, String::from_utf8(output)?))
}

const fn idle(mode: MamRequestMode) -> State {
    State::Ready(MamPreferenceState {
        saved: Some(mode),
        requested: MamPreferenceUpdate::Set(mode),
        effective: mode,
        persistence: MamPreferencePersistence::Idle,
    })
}

#[tokio::test]
async fn preferences_survive_restart_and_deletion_defaults_to_paused() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    assert_eq!(
        run(&path, StartupMode::Initialise, b"").await?.0,
        State::Ready(MamPreferenceState {
            saved: None,
            requested: MamPreferenceUpdate::Set(MamRequestMode::Paused),
            effective: MamRequestMode::Paused,
            persistence: MamPreferencePersistence::Idle,
        })
    );
    assert_eq!(
        run(&path, StartupMode::Resume, b"enable\n").await?.0,
        idle(MamRequestMode::Enabled)
    );
    assert_eq!(
        run(&path, StartupMode::Resume, b"").await?.0,
        idle(MamRequestMode::Enabled)
    );
    assert_eq!(
        run(&path, StartupMode::Resume, b"pause\n").await?.0,
        idle(MamRequestMode::Paused)
    );
    assert_eq!(
        run(&path, StartupMode::Resume, b"forget\n").await?.0,
        State::Ready(MamPreferenceState {
            saved: None,
            requested: MamPreferenceUpdate::Delete,
            effective: MamRequestMode::Paused,
            persistence: MamPreferencePersistence::Idle
        })
    );
    assert_eq!(
        run(&path, StartupMode::Resume, b"").await?.0,
        State::Ready(MamPreferenceState {
            saved: None,
            requested: MamPreferenceUpdate::Set(MamRequestMode::Paused),
            effective: MamRequestMode::Paused,
            persistence: MamPreferencePersistence::Idle,
        })
    );
    let connection = Connection::open(&path)?;
    let count: i64 = connection.query_row("SELECT COUNT(*) FROM mam_configuration", [], |row| {
        row.get(0)
    })?;
    assert_eq!(count, 0);
    Ok(())
}

#[tokio::test]
async fn setup_does_not_overwrite_and_resume_does_not_create_storage() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    assert_eq!(run(&path, StartupMode::Resume, b"").await?.0, State::Failed);
    assert!(!path.exists());
    run(&path, StartupMode::Initialise, b"enable\n").await?;
    assert_eq!(
        run(&path, StartupMode::Initialise, b"").await?.0,
        State::Failed
    );
    assert_eq!(
        run(&path, StartupMode::Resume, b"").await?.0,
        idle(MamRequestMode::Enabled)
    );
    Ok(())
}

#[tokio::test]
async fn malformed_preferences_and_failed_writes_are_reported() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    run(&path, StartupMode::Initialise, b"").await?;
    let connection = Connection::open(&path)?;
    connection.execute_batch("CREATE TRIGGER reject_write BEFORE INSERT ON mam_configuration BEGIN SELECT RAISE(ABORT, 'write rejected'); END;")?;
    let (status, output) = run(&path, StartupMode::Resume, b"enable\n").await?;
    assert_eq!(
        status,
        State::Ready(MamPreferenceState {
            saved: None,
            requested: MamPreferenceUpdate::Set(MamRequestMode::Enabled),
            effective: MamRequestMode::Paused,
            persistence: MamPreferencePersistence::Failed
        })
    );
    assert!(output.contains("write rejected"));
    connection.execute_batch("DROP TABLE mam_configuration; CREATE TABLE mam_configuration(singleton INTEGER PRIMARY KEY, request_mode TEXT); INSERT INTO mam_configuration VALUES(1, 'invalid');")?;
    let (status, output) = run(&path, StartupMode::Resume, b"").await?;
    assert_eq!(status, State::Failed);
    assert!(output.contains("invalid MAM request preference"));
    Ok(())
}

async fn wait_for<R: tokio::io::AsyncBufRead + Unpin>(
    lines: &mut tokio::io::Lines<R>,
    text: &str,
) -> TestResult {
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = lines.next_line().await? {
            if line.contains(text) {
                return Ok(());
            }
        }
        Err(std::io::Error::other(
            "console closed before expected status",
        ))
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn pause_remains_responsive_during_a_blocked_sqlite_write() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    run(&path, StartupMode::Initialise, b"").await?;
    let (client, server) = tokio::io::duplex(8192);
    let (read, mut write) = tokio::io::split(client);
    let mut lines = BufReader::new(read).lines();
    let (input, output) = tokio::io::split(server);
    let worker_path = path.clone();
    let worker = tokio::spawn(async move {
        console::run(
            &worker_path,
            StartupMode::Resume,
            BufReader::new(input),
            output,
        )
        .await
    });
    wait_for(&mut lines, "persistence: Idle").await?;
    let connection = Connection::open(&path)?;
    connection.execute_batch("BEGIN IMMEDIATE")?;
    write.write_all(b"enable\n").await?;
    wait_for(&mut lines, "requested: Set(Enabled)").await?;
    write.write_all(b"pause\n").await?;
    wait_for(
        &mut lines,
        "requested: Set(Paused), effective: Paused, persistence: Pending(Set(Enabled))",
    )
    .await?;
    connection.execute_batch("COMMIT")?;
    wait_for(&mut lines, "persistence: Idle").await?;
    write.write_all(b"status\nunknown\nquit\n").await?;
    let status = tokio::time::timeout(Duration::from_secs(10), worker).await???;
    assert_eq!(status, idle(MamRequestMode::Paused));
    assert_eq!(
        run(&path, StartupMode::Resume, b"").await?.0,
        idle(MamRequestMode::Paused)
    );
    Ok(())
}

#[tokio::test]
async fn failed_delete_keeps_requests_paused() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    run(&path, StartupMode::Initialise, b"enable\n").await?;
    Connection::open(&path)?.execute_batch("CREATE TRIGGER reject_delete BEFORE DELETE ON mam_configuration BEGIN SELECT RAISE(ABORT, 'delete rejected'); END;")?;
    let (status, output) = run(&path, StartupMode::Resume, b"forget\n").await?;
    assert_eq!(
        status,
        State::Ready(MamPreferenceState {
            saved: Some(MamRequestMode::Enabled),
            requested: MamPreferenceUpdate::Delete,
            effective: MamRequestMode::Paused,
            persistence: MamPreferencePersistence::Failed
        })
    );
    assert!(output.contains("delete rejected"));
    Ok(())
}

#[tokio::test]
async fn blocked_console_output_does_not_delay_a_pause_save() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    run(&path, StartupMode::Initialise, b"enable\n").await?;
    let (mut reader, writer) = tokio::io::duplex(1);
    let worker_path = path.clone();
    let worker = tokio::spawn(async move {
        console::run(
            &worker_path,
            StartupMode::Resume,
            &b"pause\nquit\n"[..],
            writer,
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let saved: String = Connection::open(&path)?.query_row(
                "SELECT request_mode FROM mam_configuration WHERE singleton = 1",
                [],
                |row| row.get(0),
            )?;
            if saved == "paused" {
                return Ok::<(), rusqlite::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await??;
    assert!(!worker.is_finished());
    let mut output = String::new();
    tokio::time::timeout(Duration::from_secs(10), reader.read_to_string(&mut output)).await??;
    let state = tokio::time::timeout(Duration::from_secs(10), worker).await???;
    assert_eq!(state, idle(MamRequestMode::Paused));
    assert!(output.contains("effective: Paused, persistence: Idle"));
    Ok(())
}

#[tokio::test]
async fn console_stream_errors_are_returned_to_the_caller() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    run(&path, StartupMode::Initialise, b"").await?;
    let invalid_input = console::run(
        &path,
        StartupMode::Resume,
        &b"\xff\n"[..],
        tokio::io::sink(),
    )
    .await;
    assert!(
        matches!(invalid_input, Err(error) if error.downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::InvalidData))
    );

    let (reader, writer) = tokio::io::duplex(1);
    drop(reader);
    let disconnected_output =
        console::run(&path, StartupMode::Resume, &b"status\nquit\n"[..], writer).await;
    assert!(
        matches!(disconnected_output, Err(error) if error.downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::BrokenPipe))
    );
    Ok(())
}
