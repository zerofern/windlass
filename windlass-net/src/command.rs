//! Subprocess command runner.
//!
//! Single chokepoint for every external command Windlass spawns
//! (`wg`, `ip`, `nft`).  Routing all privileged invocations through
//! one type buys us:
//!
//! - Observability via the existing [`HttpTap`]: every spawn is
//!   captured as an [`HttpExchange`] (with `method = "spawn"`,
//!   `module = <program>`, `url = <argv>`, `response_status = <exit>`,
//!   `response_body = <stdout>`, stderr in a response header) so the
//!   `/observability` UI surfaces tunnel-side privileged ops in the
//!   same ring as MAM/qBit HTTP exchanges.  `stdin` contents are
//!   never recorded — only their byte length — so the `WireGuard`
//!   private key (fed via `wg set ... private-key /dev/stdin`)
//!   cannot reach the tap.
//! - A clean seam for tests: [`Runner`] is a trait, so tests pass
//!   a recording fake and assert on the exact argv that would have
//!   been spawned without ever touching the kernel.
//! - A clear migration path: when we swap subprocess for netlink, the
//!   call sites change from `runner.run("wg", &[...])` to typed
//!   netlink ops on a `WgHandle` — same shape, same return error
//!   surface.

use std::ffi::OsString;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use tracing::debug;
use windlass_types::{CoreId, HttpExchange, HttpRequestView, HttpTap};

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

/// Production runner that spawns via `tokio::process::Command` and
/// captures every spawn through the [`HttpTap`] observability hook.
pub struct SystemRunner {
    core_id: CoreId,
    tap: Arc<dyn HttpTap>,
}

impl SystemRunner {
    #[must_use]
    pub fn new(core_id: CoreId, tap: Arc<dyn HttpTap>) -> Self {
        Self { core_id, tap }
    }

    /// Convenience for tests and call sites that don't want
    /// observability.
    #[must_use]
    pub fn null() -> Self {
        Self {
            core_id: CoreId::Tunnel,
            tap: windlass_types::NullHttpTap::arc(),
        }
    }

    /// Joins program + argv into a single string for the captured
    /// exchange's `url` field.
    fn url_for(program: &str, args: &[&str]) -> String {
        let mut out =
            String::with_capacity(program.len() + args.iter().map(|a| a.len() + 1).sum::<usize>());
        out.push_str(program);
        for a in args {
            out.push(' ');
            out.push_str(a);
        }
        out
    }

    /// Builds the exchange to record after a finished spawn.
    /// `stdin_len` is the ONLY stdin information we ever surface —
    /// the contents never reach the tap.
    fn exchange(
        program: &str,
        url: &str,
        stdin_len: Option<usize>,
        result: &Result<CommandOutcome, CommandError>,
        output: &std::process::Output,
    ) -> HttpExchange {
        // The HttpExchange status is u16; subprocess exit codes are
        // i32 in range 0..=255 typically.  Clamp and cast.
        let raw_code = output.status.code().unwrap_or(-1);
        let response_status = u16::try_from(raw_code.max(0).min(i32::from(u16::MAX))).unwrap_or(0);
        let mut request_headers = Vec::new();
        if let Some(n) = stdin_len {
            request_headers.push(("stdin-bytes".to_string(), n.to_string()));
        }
        let response_body = result
            .as_ref()
            .map_or_else(|_| String::new(), |o| o.stdout.clone());
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let response_headers = if stderr.is_empty() {
            Vec::new()
        } else {
            vec![("stderr".to_string(), stderr)]
        };
        HttpExchange {
            module: program.to_string(),
            method: "spawn".to_string(),
            url: url.to_string(),
            request_headers,
            request_body: None,
            response_status,
            response_headers,
            response_body,
        }
    }
}

#[async_trait]
impl Runner for SystemRunner {
    async fn run(&self, program: &str, args: &[&str]) -> Result<CommandOutcome, CommandError> {
        debug!(program, ?args, "spawn");
        let url = Self::url_for(program, args);
        self.tap
            .gate_request(
                self.core_id,
                &HttpRequestView {
                    method: "spawn",
                    url: &url,
                    body: None,
                },
            )
            .await;
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
        let result = finish(program, &output);
        self.tap.observed_exchange(
            self.core_id,
            &Self::exchange(program, &url, None, &result, &output),
        );
        result
    }

    async fn run_with_stdin(
        &self,
        program: &str,
        args: &[&str],
        stdin: &str,
    ) -> Result<CommandOutcome, CommandError> {
        use tokio::io::AsyncWriteExt as _;
        debug!(program, ?args, stdin_len = stdin.len(), "spawn with stdin");
        let url = Self::url_for(program, args);
        self.tap
            .gate_request(
                self.core_id,
                &HttpRequestView {
                    method: "spawn",
                    url: &url,
                    body: None,
                },
            )
            .await;
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
        let result = finish(program, &output);
        self.tap.observed_exchange(
            self.core_id,
            &Self::exchange(program, &url, Some(stdin.len()), &result, &output),
        );
        result
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
        let r = SystemRunner::null();
        let out = r.run("echo", &["hello"]).await.expect("echo succeeds");
        assert!(out.stdout.contains("hello"));
        assert_eq!(out.exit_code, 0);
    }

    #[tokio::test]
    async fn system_runner_surfaces_nonzero_exit() {
        let r = SystemRunner::null();
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
        let r = SystemRunner::null();
        let err = r
            .run("this-binary-does-not-exist-zzz", &[])
            .await
            .expect_err("missing binary should fail to spawn");
        assert!(matches!(err, CommandError::Spawn { .. }));
    }

    #[tokio::test]
    async fn system_runner_pipes_stdin_to_process() {
        let r = SystemRunner::null();
        let out = r
            .run_with_stdin("cat", &[], "stdin-content")
            .await
            .expect("cat succeeds");
        assert_eq!(out.stdout, "stdin-content");
    }

    /// Captures every observed exchange so a test can assert on the
    /// `module` + `url` + redaction shape.
    #[derive(Default)]
    struct RecordingTap {
        gated: std::sync::Mutex<Vec<(CoreId, String, String)>>,
        observed: std::sync::Mutex<Vec<HttpExchange>>,
    }

    #[async_trait]
    impl HttpTap for RecordingTap {
        async fn gate_request(&self, core: CoreId, view: &HttpRequestView<'_>) {
            self.gated
                .lock()
                .unwrap()
                .push((core, view.method.to_string(), view.url.to_string()));
        }
        fn observed_exchange(&self, _core: CoreId, exchange: &HttpExchange) {
            self.observed.lock().unwrap().push(exchange.clone());
        }
        fn signal_anomaly(&self, _core: CoreId, _anomaly: windlass_types::HttpAnomaly) {}
    }

    #[tokio::test]
    async fn spawn_is_captured_as_exchange() {
        let tap = Arc::new(RecordingTap::default());
        let r = SystemRunner::new(CoreId::Tunnel, tap.clone());
        let _ = r.run("echo", &["hello"]).await.unwrap();
        let gated_snapshot = tap.gated.lock().unwrap().clone();
        assert_eq!(gated_snapshot.len(), 1);
        assert_eq!(gated_snapshot[0].0, CoreId::Tunnel);
        assert_eq!(gated_snapshot[0].1, "spawn");
        assert!(gated_snapshot[0].2.starts_with("echo "));
        let observed_snapshot = tap.observed.lock().unwrap().clone();
        assert_eq!(observed_snapshot.len(), 1);
        assert_eq!(observed_snapshot[0].module, "echo");
        assert!(observed_snapshot[0].response_body.contains("hello"));
    }

    #[tokio::test]
    async fn stdin_contents_are_never_captured_only_length() {
        // Critical test: the WireGuard private key flows via stdin
        // (`wg set ... private-key /dev/stdin`).  The captured
        // exchange must not contain those bytes anywhere.
        let tap = Arc::new(RecordingTap::default());
        let r = SystemRunner::new(CoreId::Tunnel, tap.clone());
        let secret = "REAL-WIREGUARD-PRIVATE-KEY-CLEARTEXT";
        let _ = r.run_with_stdin("cat", &[], secret).await.unwrap();
        let observed_snapshot = tap.observed.lock().unwrap().clone();
        let ex = &observed_snapshot[0];
        assert!(ex.request_body.is_none());
        // No header value contains the secret.
        for (_, v) in &ex.request_headers {
            assert!(!v.contains(secret), "header leaked secret: {v}");
        }
        // The stdin-bytes header carries the LENGTH only.
        let stdin_bytes = ex
            .request_headers
            .iter()
            .find(|(k, _)| k == "stdin-bytes")
            .expect("stdin-bytes header present");
        assert_eq!(stdin_bytes.1, secret.len().to_string());
        // Response body (stdout) intentionally echoes the stdin via
        // cat — that's an artefact of using cat as a test fixture
        // and not a real-world leak path because production calls
        // (`wg set`) do not echo their stdin.  Confirm via the
        // request side that the tap NEVER sees the secret.
    }
}
