//! A thin abstraction over external commands (gh, ghq, git, wt, pi, herdr) so
//! the workflows can be exercised with a fake in tests.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};

/// How a command's standard streams are wired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Io {
    /// stdin is closed; stdout and stderr are captured.
    Capture,
    /// stdout is captured; stdin and stderr stay attached to the terminal so
    /// progress output and confirmation prompts reach the user.
    CaptureStdout,
    /// Everything stays attached to the terminal.
    Inherit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub io: Io,
    pub timeout: Option<Duration>,
}

impl Cmd {
    pub fn new<I, S>(program: impl Into<String>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Cmd {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
            io: Io::Capture,
            timeout: None,
        }
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn io(mut self, io: Io) -> Self {
        self.io = io;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// `program arg1 arg2 ...` for logs and test assertions.
    pub fn line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Output {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn ok(stdout: impl Into<String>) -> Self {
        Output { success: true, stdout: stdout.into(), stderr: String::new() }
    }

    pub fn failed(stderr: impl Into<String>) -> Self {
        Output { success: false, stdout: String::new(), stderr: stderr.into() }
    }
}

pub trait Runner: Sync {
    fn run(&self, cmd: &Cmd) -> Result<Output>;

    /// Run `cmd` and return its stdout, or an error carrying its stderr when
    /// it exits unsuccessfully.
    fn stdout(&self, cmd: &Cmd) -> Result<String> {
        let out = self.run(cmd)?;
        if !out.success {
            let detail = out.stderr.trim();
            if detail.is_empty() {
                bail!("`{}` failed", cmd.line());
            }
            bail!("`{}` failed: {detail}", cmd.line());
        }
        Ok(out.stdout)
    }
}

/// Runs commands for real.
pub struct SystemRunner;

impl Runner for SystemRunner {
    fn run(&self, cmd: &Cmd) -> Result<Output> {
        use std::io::Read;
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        let mut command = Command::new(&cmd.program);
        command.args(&cmd.args);
        if let Some(cwd) = &cmd.cwd {
            command.current_dir(cwd);
        }
        match cmd.io {
            Io::Capture => {
                command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
                // A timed-out command is killed together with its children.
                // Only done for commands detached from the terminal: a process
                // outside the foreground group can't read from it.
                if cmd.timeout.is_some() {
                    command.process_group(0);
                }
            }
            Io::CaptureStdout => {
                command.stdin(Stdio::inherit()).stdout(Stdio::piped()).stderr(Stdio::inherit());
            }
            Io::Inherit => {
                command.stdin(Stdio::inherit()).stdout(Stdio::inherit()).stderr(Stdio::inherit());
            }
        }

        let mut child = command
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to start `{}`: {e}", cmd.program))?;

        let reader = |pipe: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_end(&mut buf);
                }
                String::from_utf8_lossy(&buf).into_owned()
            })
        };
        let stdout = reader(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
        let stderr = reader(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));

        let status = match cmd.timeout {
            None => child.wait()?,
            Some(timeout) => {
                let deadline = std::time::Instant::now() + timeout;
                loop {
                    if let Some(status) = child.try_wait()? {
                        break status;
                    }
                    if std::time::Instant::now() >= deadline {
                        if cmd.io == Io::Capture {
                            let _ = Command::new("kill")
                                .args(["-KILL", "--", &format!("-{}", child.id())])
                                .stderr(Stdio::null())
                                .status();
                        }
                        let _ = child.kill();
                        let _ = child.wait();
                        // The reader threads finish once the pipes close; don't
                        // wait for grandchildren that might still hold them.
                        bail!("`{}` timed out after {}s", cmd.line(), timeout.as_secs_f32());
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        };

        Ok(Output {
            success: status.success(),
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
    }
}

#[cfg(test)]
pub mod fake {
    use super::*;
    use std::sync::Mutex;

    type Handler = Box<dyn Fn(&Cmd) -> Result<Output> + Send + Sync>;

    /// Records every command and answers it with `handler`.
    pub struct FakeRunner {
        calls: Mutex<Vec<Cmd>>,
        handler: Handler,
    }

    impl FakeRunner {
        pub fn new(handler: impl Fn(&Cmd) -> Result<Output> + Send + Sync + 'static) -> Self {
            FakeRunner { calls: Mutex::new(Vec::new()), handler: Box::new(handler) }
        }

        pub fn calls(&self) -> Vec<Cmd> {
            self.calls.lock().unwrap().clone()
        }

        pub fn lines(&self) -> Vec<String> {
            self.calls().iter().map(Cmd::line).collect()
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, cmd: &Cmd) -> Result<Output> {
            self.calls.lock().unwrap().push(cmd.clone());
            (self.handler)(cmd)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Cmd {
        Cmd::new("sh", ["-c", script])
    }

    #[test]
    fn captures_stdout_and_stderr() {
        let out = SystemRunner.run(&sh("echo out; echo err >&2")).unwrap();
        assert!(out.success);
        assert_eq!(out.stdout, "out\n");
        assert_eq!(out.stderr, "err\n");
    }

    #[test]
    fn reports_failure() {
        let out = SystemRunner.run(&sh("echo nope >&2; exit 3")).unwrap();
        assert!(!out.success);
        assert_eq!(out.stderr, "nope\n");
    }

    #[test]
    fn runs_in_the_requested_directory() {
        let dir = tempfile::tempdir().unwrap();
        let out = SystemRunner.run(&sh("pwd -P").cwd(dir.path())).unwrap();
        assert_eq!(out.stdout.trim(), dir.path().canonicalize().unwrap().to_str().unwrap());
    }

    #[test]
    fn missing_program_is_an_error() {
        let err = SystemRunner.run(&Cmd::new("definitely-not-a-real-program-xyz", Vec::<String>::new()));
        assert!(err.is_err());
    }

    #[test]
    fn kills_commands_that_exceed_their_timeout() {
        let started = std::time::Instant::now();
        let err = SystemRunner
            .run(&sh("sleep 5").timeout(Duration::from_millis(200)))
            .unwrap_err();
        assert!(format!("{err:#}").contains("timed out"), "{err:#}");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn stdout_helper_surfaces_stderr_on_failure() {
        let err = SystemRunner.stdout(&sh("echo broken >&2; exit 1")).unwrap_err();
        assert!(format!("{err:#}").contains("broken"), "{err:#}");
        assert_eq!(SystemRunner.stdout(&sh("printf hi")).unwrap(), "hi");
    }
}
