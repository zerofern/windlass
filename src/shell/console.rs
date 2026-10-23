//! Feed console requests into the runner and display its read-only observations.

use std::{io, path::Path};

use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, Lines},
    sync::watch,
};

use crate::{
    core::{Event, MamPauseEvent, MamRequestMode, StartupEvent, StartupMode, State},
    runner::{Observation, Runner},
};

use super::{EventSender, ShellResult};

const HELP: &str = "Commands: pause, enable, forget, status, quit\n";

enum Command {
    Event(Event),
    Status,
    Help,
    Quit,
}

/// Run the inspection console with the same ingress available to other producers.
///
/// Commands are `pause`, `enable`, `forget`, `status`, and `quit`.
/// Slow output keeps the latest status and storage error while the runner proceeds.
/// Normal shutdown drains accepted inputs, storage effects, and console output.
///
/// # Errors
/// Returns an error if console I/O or a worker fails unexpectedly. Ordinary storage
/// failures are delivered to the core and displayed in its observations.
pub async fn run(
    database: &Path,
    mode: StartupMode,
    input: impl AsyncBufRead + Unpin,
    output: impl AsyncWrite + Unpin,
) -> ShellResult<State> {
    let (runner, events, observations) = Runner::new(database.to_owned());
    let (updates, receiver) = watch::channel(render(&observations.borrow()));
    let writer = async { write_output(receiver, output).await.map_err(Into::into) };
    let reader = async {
        drive(mode, input, events, observations, updates)
            .await
            .map_err(Into::into)
    };
    tokio::try_join!(runner.run(), reader, writer).map(|(state, (), ())| state)
}

async fn drive(
    mode: StartupMode,
    input: impl AsyncBufRead + Unpin,
    events: EventSender,
    mut observations: watch::Receiver<Observation>,
    updates: watch::Sender<String>,
) -> io::Result<()> {
    events
        .send(Event::Startup(StartupEvent::Start { mode }))
        .await?;
    let mut lines = input.lines();
    loop {
        tokio::select! {
            command = read_command(&mut lines) => match command? {
                Command::Quit => break,
                Command::Event(event) => events.send(event).await?,
                Command::Status => { updates.send_replace(render(&observations.borrow())); }
                Command::Help => { updates.send_replace(format!("{HELP}{}", render(&observations.borrow()))); }
            },
            changed = observations.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                updates.send_replace(render(&observations.borrow_and_update()));
            }
        }
    }
    events.shutdown().await?;
    while observations.changed().await.is_ok() {
        updates.send_replace(render(&observations.borrow_and_update()));
    }
    Ok(())
}

async fn read_command(lines: &mut Lines<impl AsyncBufRead + Unpin>) -> io::Result<Command> {
    let Some(line) = lines.next_line().await? else {
        return Ok(Command::Quit);
    };
    Ok(match line.trim() {
        "pause" => Command::Event(Event::MamPause(MamPauseEvent::Set {
            mode: MamRequestMode::Paused,
        })),
        "enable" => Command::Event(Event::MamPause(MamPauseEvent::Set {
            mode: MamRequestMode::Enabled,
        })),
        "forget" => Command::Event(Event::MamPause(MamPauseEvent::Delete)),
        "status" => Command::Status,
        "quit" => Command::Quit,
        _ => Command::Help,
    })
}

fn render(observation: &Observation) -> String {
    let state = &observation.state;
    observation.storage_error.as_ref().map_or_else(
        || format!("{state:?}\n"),
        |detail| format!("Storage error: {detail}\n{state:?}\n"),
    )
}

async fn write_output(
    mut updates: watch::Receiver<String>,
    mut output: impl AsyncWrite + Unpin,
) -> io::Result<()> {
    while updates.changed().await.is_ok() {
        // Release the watch borrow before waiting on the output stream.
        let snapshot = updates.borrow_and_update().clone();
        output.write_all(snapshot.as_bytes()).await?;
        output.flush().await?;
    }
    Ok(())
}
