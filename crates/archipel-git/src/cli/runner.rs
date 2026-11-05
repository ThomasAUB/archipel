//! Running `git` subprocesses: timeouts, cancellation, progress, logging.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use archipel_model::{CancelToken, GitError, ProgressSink};

use crate::parse::progress::{parse_progress, split_progress_lines};

/// Receives each streamed stdout record; `Break` stops the process.
pub(crate) type RecordSink<'a> = dyn FnMut(&[u8]) -> Result<ControlFlow<()>, GitError> + 'a;

/// Whether a command only reads, writes locally, or uses the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CmdKind {
    /// Read-only: runs with `--no-optional-locks`.
    Read,
    /// Local write.
    Write,
    /// Network operation (longer timeout, progress).
    Network,
}

/// The output of a finished command.
pub(crate) struct CmdOutput {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

impl CmdOutput {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).trim_end().to_owned()
    }
}

/// A command to run.
pub(crate) struct Cmd<'a> {
    pub repo: Option<&'a Path>,
    pub args: Vec<OsString>,
    /// Extra environment variables.
    pub env: Vec<(OsString, OsString)>,
    pub kind: CmdKind,
    pub stdin: Option<Vec<u8>>,
    pub cancel: Option<&'a CancelToken>,
    pub progress: Option<&'a dyn ProgressSink>,
    /// Exit codes treated as success (default: only 0).
    pub ok_codes: &'static [i32],
}

impl<'a> Cmd<'a> {
    pub fn new(repo: &'a Path, kind: CmdKind, args: &[&str]) -> Self {
        Self {
            repo: Some(repo),
            args: args.iter().map(OsString::from).collect(),
            env: Vec::new(),
            kind,
            stdin: None,
            cancel: None,
            progress: None,
            ok_codes: &[0],
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I: IntoIterator<Item = S>, S: Into<OsString>>(mut self, args: I) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn stdin(mut self, data: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(data.into());
        self
    }

    pub fn cancel(mut self, cancel: &'a CancelToken) -> Self {
        self.cancel = Some(cancel);
        self
    }

    pub fn progress(mut self, progress: &'a dyn ProgressSink) -> Self {
        self.progress = Some(progress);
        self
    }

    pub fn ok_codes(mut self, codes: &'static [i32]) -> Self {
        self.ok_codes = codes;
        self
    }

    fn display(&self) -> String {
        let mut s = String::from("git");
        for a in &self.args {
            s.push(' ');
            s.push_str(&a.to_string_lossy());
        }
        s
    }
}

/// Spawns and supervises `git` processes.
#[derive(Clone, Debug)]
pub(crate) struct Runner {
    pub program: PathBuf,
    pub local_timeout: Duration,
    pub network_timeout: Duration,
    pub askpass: Option<PathBuf>,
    pub credential_helper: Option<String>,
}

impl Runner {
    fn command(&self, cmd: &Cmd<'_>) -> Command {
        let mut c = Command::new(&self.program);
        if let Some(repo) = cmd.repo {
            c.current_dir(repo);
        }
        c.env("GIT_TERMINAL_PROMPT", "0")
            // Never open an interactive editor; hooks still run.
            .env("GIT_EDITOR", "true")
            .args(["-c", "color.ui=false", "-c", "core.quotepath=false"]);
        if cmd.kind == CmdKind::Read {
            c.arg("--no-optional-locks");
        }
        // `-c` entries come after every config file: git asks the user's
        // helpers first.
        if let Some(helper) = &self.credential_helper {
            c.arg("-c").arg(format!("credential.helper={helper}"));
        }
        if let Some(askpass) = &self.askpass {
            c.env("GIT_ASKPASS", askpass)
                .env("SSH_ASKPASS", askpass)
                .env("SSH_ASKPASS_REQUIRE", "force");
        }
        c.envs(cmd.env.iter().map(|(k, v)| (k, v)));
        c.args(&cmd.args);
        c.stdin(if cmd.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        hide_console(&mut c);
        c
    }

    fn spawn(&self, cmd: &mut Cmd<'_>) -> Result<Child, GitError> {
        let mut child = self.command(cmd).spawn().map_err(|e| GitError::Io {
            path: self.program.clone(),
            message: format!("cannot run git: {e}"),
        })?;
        if let (Some(data), Some(mut stdin)) = (cmd.stdin.take(), child.stdin.take()) {
            // A writer thread avoids deadlocks when stdout fills up first.
            thread::spawn(move || {
                // A failed write surfaces as the command's own error.
                let _ = stdin.write_all(&data);
            });
        }
        Ok(child)
    }

    fn timeout(&self, kind: CmdKind) -> Duration {
        if kind == CmdKind::Network {
            self.network_timeout
        } else {
            self.local_timeout
        }
    }

    /// Runs a command to completion and returns its output, or an error when
    /// the exit code is not in `ok_codes`.
    pub fn run(&self, mut cmd: Cmd<'_>) -> Result<CmdOutput, GitError> {
        let started = Instant::now();
        let display = cmd.display();
        let mut child = self.spawn(&mut cmd)?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let progress = cmd.progress;
        let deadline = started + self.timeout(cmd.kind);
        let result = thread::scope(|s| {
            let out = s.spawn(move || read_all(stdout));
            let err = s.spawn(move || read_stderr(stderr, progress));
            let status = supervise(&mut child, cmd.cancel, deadline, self.timeout(cmd.kind));
            let stdout = out.join().unwrap_or_default();
            let stderr = err.join().unwrap_or_default();
            status.map(|code| CmdOutput {
                code,
                stdout,
                stderr,
            })
        });
        let output = result?;
        log_command(&display, started, output.code);
        if output.code.is_some_and(|c| cmd.ok_codes.contains(&c)) {
            Ok(output)
        } else {
            Err(GitError::CommandFailed {
                command: display,
                exit_code: output.code,
                stderr: output.stderr.trim().to_owned(),
            })
        }
    }

    /// Runs a command and streams its stdout split on `delimiter`; the sink
    /// may stop early with `Break` (the process is then killed).
    pub fn stream(
        &self,
        mut cmd: Cmd<'_>,
        delimiter: u8,
        sink: &mut RecordSink<'_>,
    ) -> Result<(), GitError> {
        let started = Instant::now();
        let display = cmd.display();
        let mut child = self.spawn(&mut cmd)?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let err_thread = thread::spawn(move || read_stderr(stderr, None));
        let outcome = pump(stdout, delimiter, cmd.cancel, sink);
        if !matches!(outcome, Ok(ControlFlow::Continue(()))) {
            // Early stop or failure: no need to let git finish.
            let _ = child.kill();
        }
        let status = child.wait().map_err(|e| GitError::Backend(e.to_string()))?;
        let stderr = err_thread.join().unwrap_or_default();
        log_command(&display, started, status.code());
        match outcome? {
            ControlFlow::Break(()) => Ok(()),
            ControlFlow::Continue(()) if status.success() => Ok(()),
            ControlFlow::Continue(()) => Err(GitError::CommandFailed {
                command: display,
                exit_code: status.code(),
                stderr: stderr.trim().to_owned(),
            }),
        }
    }
}

fn pump(
    stdout: Option<ChildStdout>,
    delimiter: u8,
    cancel: Option<&CancelToken>,
    sink: &mut RecordSink<'_>,
) -> Result<ControlFlow<()>, GitError> {
    let Some(stdout) = stdout else {
        return Ok(ControlFlow::Continue(()));
    };
    let mut reader = BufReader::with_capacity(64 * 1024, stdout);
    let mut record = Vec::with_capacity(256);
    loop {
        record.clear();
        let n = reader
            .read_until(delimiter, &mut record)
            .map_err(|e| GitError::Backend(e.to_string()))?;
        if n == 0 {
            return Ok(ControlFlow::Continue(()));
        }
        if let Some(c) = cancel {
            c.check()?;
        }
        if record.last() == Some(&delimiter) {
            record.pop();
        }
        if sink(&record)?.is_break() {
            return Ok(ControlFlow::Break(()));
        }
    }
}

/// Waits for the child, killing it on cancellation or timeout.
fn supervise(
    child: &mut Child,
    cancel: Option<&CancelToken>,
    deadline: Instant,
    timeout: Duration,
) -> Result<Option<i32>, GitError> {
    let mut delay = Duration::from_micros(500);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.code()),
            Ok(None) => {}
            Err(e) => return Err(GitError::Backend(e.to_string())),
        }
        if cancel.is_some_and(CancelToken::is_cancelled) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(GitError::Cancelled);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(GitError::TimedOut {
                seconds: timeout.as_secs(),
            });
        }
        thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_millis(20));
    }
}

fn read_all(stdout: Option<ChildStdout>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut s) = stdout {
        // A read error leaves a partial buffer; the exit code reports failure.
        let _ = s.read_to_end(&mut buf);
    }
    buf
}

fn read_stderr(
    stderr: Option<std::process::ChildStderr>,
    progress: Option<&dyn ProgressSink>,
) -> String {
    let Some(mut stderr) = stderr else {
        return String::new();
    };
    let mut all = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut pending = String::new();
    while let Ok(n) = stderr.read(&mut chunk) {
        if n == 0 {
            break;
        }
        all.extend_from_slice(&chunk[..n]);
        if let Some(sink) = progress {
            pending.push_str(&String::from_utf8_lossy(&chunk[..n]));
            if let Some(cut) = pending.rfind(['\r', '\n']) {
                for line in split_progress_lines(&pending[..cut]) {
                    if let Some(p) = parse_progress(line) {
                        sink.report(p);
                    }
                }
                pending.drain(..=cut);
            }
        }
    }
    String::from_utf8_lossy(&all).into_owned()
}

fn log_command(command_line: &str, started: Instant, code: Option<i32>) {
    tracing::debug!(
        target: "archipel_git::cli",
        command = command_line,
        duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        exit_code = code,
        "git command"
    );
}

#[cfg(windows)]
fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_console(_cmd: &mut Command) {}
