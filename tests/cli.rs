//! Exercise the actual console executable and its persistent `SQLite` preference.

use std::{
    error::Error,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

fn invoke(mode: &str, database: &Path, commands: &[u8]) -> Result<Output, Box<dyn Error>> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_windlass"))
        .arg(mode)
        .arg(database)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("missing console input"))?;
    input.write_all(commands)?;
    drop(input);
    Ok(child.wait_with_output()?)
}

#[test]
fn console_persists_commands_and_restores_them_on_restart() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    let setup = invoke("initialise", &path, b"enable\nquit\n")?;
    assert!(
        setup.status.success(),
        "{}",
        String::from_utf8_lossy(&setup.stderr)
    );
    assert!(String::from_utf8(setup.stdout)?.contains("effective: Enabled, persistence: Idle"));
    let resumed = invoke("resume", &path, b"status\nquit\n")?;
    assert!(resumed.status.success());
    assert!(String::from_utf8(resumed.stdout)?.contains("effective: Enabled, persistence: Idle"));
    let overwrite = invoke("initialise", &path, b"")?;
    assert!(!overwrite.status.success());
    let missing = invoke("resume", &directory.path().join("missing.sqlite"), b"")?;
    assert!(!missing.status.success());
    Ok(())
}

#[test]
fn invalid_arguments_do_not_create_storage() -> Result<(), Box<dyn Error>> {
    for arguments in [
        vec![],
        vec!["invalid"],
        vec!["initialise"],
        vec!["resume", "unused.sqlite", "extra"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_windlass"))
            .args(arguments)
            .output()?;
        assert!(!output.status.success());
    }
    Ok(())
}

#[test]
fn failed_preference_updates_exit_unsuccessfully() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    assert!(
        invoke("initialise", &path, b"pause\nquit\n")?
            .status
            .success()
    );
    let connection = rusqlite::Connection::open(&path)?;
    connection.execute_batch("CREATE TRIGGER reject_write BEFORE INSERT ON mam_configuration BEGIN SELECT RAISE(ABORT, 'write rejected'); END;")?;
    let failed_enable = invoke("resume", &path, b"enable\nquit\n")?;
    assert!(!failed_enable.status.success());
    assert!(String::from_utf8(failed_enable.stdout)?.contains("write rejected"));
    connection.execute_batch("DROP TRIGGER reject_write; CREATE TRIGGER reject_delete BEFORE DELETE ON mam_configuration BEGIN SELECT RAISE(ABORT, 'delete rejected'); END;")?;
    let failed_delete = invoke("resume", &path, b"forget\nquit\n")?;
    assert!(!failed_delete.status.success());
    assert!(String::from_utf8(failed_delete.stdout)?.contains("delete rejected"));
    Ok(())
}

#[test]
fn invalid_serve_arguments_are_refused() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let database = directory.path().join("windlass.sqlite");
    let database = database.to_str().ok_or("non-UTF-8 path")?;
    for arguments in [
        vec!["serve"],
        vec!["serve", "invalid"],
        vec!["serve", "initialise"],
        vec!["serve", "initialise", database],
        vec!["serve", "initialise", database, "--listen"],
        vec![
            "serve",
            "initialise",
            database,
            "--listen",
            "not-an-address",
        ],
        vec!["serve", "initialise", database, "--unknown", "value"],
        vec!["serve", "initialise", database, "--listen", "192.0.2.1:1"],
        vec![
            "serve",
            "initialise",
            database,
            "--qbittorrent",
            "not a url",
        ],
        vec![
            "serve",
            "initialise",
            database,
            "--nat-pmp-gateway",
            "nowhere",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_windlass"))
            .args(&arguments)
            .output()?;
        assert!(!output.status.success(), "{arguments:?}");
        assert!(!output.stderr.is_empty(), "{arguments:?}");
    }
    assert!(!Path::new(database).exists());
    Ok(())
}

fn serve_until_terminated(
    mode: &str,
    database: &Path,
    startup: std::time::Duration,
) -> Result<std::process::ExitStatus, Box<dyn Error>> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_windlass"))
        .args(["serve", mode])
        .arg(database)
        .args(["--listen", "127.0.0.1:0", "--allowed-host", "localhost"])
        // Shutdown waits for requests to integrations that never answer to time out.
        .args([
            "--qbittorrent",
            "http://127.0.0.1:9/",
            "--nat-pmp-gateway",
            "127.0.0.1",
        ])
        .stderr(Stdio::piped())
        .spawn()?;
    let started = std::time::Instant::now();
    let wal = database.with_extension("sqlite-wal");
    while !wal.exists() && started.elapsed() < startup {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Give a failing startup time to finish before asking the service to stop.
    std::thread::sleep(std::time::Duration::from_millis(200));
    let terminated = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()?;
    assert!(terminated.success());
    Ok(child.wait()?)
}

#[test]
fn the_service_stops_cleanly_on_sigterm() -> Result<(), Box<dyn Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("windlass.sqlite");
    assert!(
        serve_until_terminated("initialise", &path, std::time::Duration::from_secs(10))?.success()
    );
    let entries: i64 = rusqlite::Connection::open(&path)?.query_row(
        "SELECT COUNT(*) FROM history",
        [],
        |row| row.get(0),
    )?;
    assert!(entries >= 4, "{entries} history entries");
    let missing = directory.path().join("missing.sqlite");
    assert!(!serve_until_terminated("resume", &missing, std::time::Duration::ZERO)?.success());
    Ok(())
}
