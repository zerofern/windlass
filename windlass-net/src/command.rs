//! Subprocess command runner.
//!
//! Single chokepoint for every external command Windlass spawns
//! (`wg`, `ip`, `nft`).  Routing all privileged invocations through
//! one type buys us:
//!
//! - One place to add observability instrumentation when the
//!   tunnel-side `NetlinkTap` ships in Phase 3.
//! - A clean seam for tests: [`Runner`] is a trait, so tests pass
//!   a recording fake and assert on the exact argv that would have
//!   been spawned without ever touching the kernel.
//! - A clear migration path: when we swap subprocess for netlink, the
//!   call sites change from `runner.run("wg", &[...])` to typed
//!   netlink ops on a `WgHandle` — same shape, same return error
//!   surface.

use std::ffi::OsString;
use std::process::Stdio;

use async_trait::async_trait;
use thiserror::Error;
use tracing::debug;

/// Result of a successful command run — stdout/stderr text and the
/// exit code.  Even a successful run carries stderr because `wg show`
/// and `nft list` write diagnostic lines to stderr on success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("failed to spawn `{program}`: {source}")]
    Spawn {
        program: String,
        #[source]
        source: std::io::Error,
    },
    #[error("`{program}` exited with code {code}: {stderr}")]
    NonZeroExit {
        program: String,
        code: i32,
        stderr: String,
    },
    #[error("`{program}` killed by signal (no exit code)")]
    Signal { program: String },
}

/// Trait for spawning external commands.  The production impl is
/// [`SystemRunner`]; tests use a fake that records calls.
#[async_trait]
pub trait Runner: Send + Sync {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutcome, CommandError>;

    /// Variant that feeds `stdin` content into the process — used by
    /// `nft -f -` to apply a ruleset from a string without writing a
    /// temp file.
    async fn run_with_stdin(
        &self,
        program: &str,
        args: &[&str],
        stdin: &str,
    ) -> Result<CommandOutcome, CommandError>;
}

/// Production runner that spawns via `tokio::process::Command`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemRunner;

#[async_trait]
impl Runner for SystemRunner {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutcome, CommandError> {
        debug!(program, ?args, "spawn");
        let output = tokio::process::Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(|source| CommandError::Spawn {
                program: program.to_string(),
                source,
            })?;
        finish(program, &output)
    }

    async fn run_with_stdin(
        &self,
        program: &str,
        args: &[&str],
        stdin: &str,
    ) -> Result<CommandOutcome, CommandError> {
        use tokio::io::AsyncWriteExt as _;
        debug!(program, ?args, stdin_len = stdin.len(), "spawn with stdin");
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|source| CommandError::Spawn {
                program: program.to_string(),
                source,
            })?;
        if let Some(mut h) = child.stdin.take() {
            h.write_all(stdin.as_bytes())
                .await
                .map_err(|source| CommandError::Spawn {
                    program: program.to_string(),
                    source,
                })?;
        }
        let output = child
            .wait_with_output()
            .await
            .map_err(|source| CommandError::Spawn {
                program: program.to_string(),
                source,
            })?;
        finish(program, &output)
    }
}

fn finish(program: &str, output: &std::process::Output) -> Result<CommandOutcome, CommandError> {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let Some(code) = output.status.code() else {
        return Err(CommandError::Signal {
            program: program.to_string(),
        });
    };
    if code != 0 {
        return Err(CommandError::NonZeroExit {
            program: program.to_string(),
            code,
            stderr,
        });
    }
    Ok(CommandOutcome {
        stdout,
        stderr,
        exit_code: code,
    })
}

/// Variant of `args: &[&str]` that owns its strings — used by test
/// recorders so the recorded call survives the borrow.
#[must_use]
pub fn args_to_owned(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn system_runner_captures_stdout() {
        let r = SystemRunner;
        let out = r.run("echo", &["hello"]).await.expect("echo succeeds");
        assert!(out.stdout.contains("hello"));
        assert_eq!(out.exit_code, 0);
    }

    #[tokio::test]
    async fn system_runner_surfaces_nonzero_exit() {
        let r = SystemRunner;
        let err = r
            .run("sh", &["-c", "echo err >&2; exit 7"])
            .await
            .expect_err("non-zero should error");
        match err {
            CommandError::NonZeroExit { code, stderr, .. } => {
                assert_eq!(code, 7);
                assert!(stderr.contains("err"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn system_runner_handles_missing_binary() {
        let r = SystemRunner;
        let err = r
            .run("this-binary-does-not-exist-zzz", &[])
            .await
            .expect_err("missing binary should fail to spawn");
        assert!(matches!(err, CommandError::Spawn { .. }));
    }

    #[tokio::test]
    async fn system_runner_pipes_stdin_to_process() {
        let r = SystemRunner;
        let out = r
            .run_with_stdin("cat", &[], "stdin-content")
            .await
            .expect("cat succeeds");
        assert_eq!(out.stdout, "stdin-content");
    }
}
