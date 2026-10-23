//! Run the minimal Windlass console for inspecting core and shell interactions.

use std::{error::Error, io, path::PathBuf};

use windlass::{core::StartupMode, shell::console};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut arguments = std::env::args_os().skip(1);
    let mode = match arguments
        .next()
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
    {
        Some("initialise") => StartupMode::Initialise,
        Some("resume") => StartupMode::Resume,
        _ => {
            return Err(
                io::Error::other("usage: windlass <initialise|resume> <database-path>").into(),
            );
        }
    };
    let database = arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("missing database path"))?;
    if arguments.next().is_some() {
        return Err(io::Error::other("unexpected extra argument").into());
    }
    let status = console::run(
        &database,
        mode,
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await?;
    if status.has_failed() {
        return Err(io::Error::other("startup or preference persistence failed").into());
    }
    Ok(())
}
