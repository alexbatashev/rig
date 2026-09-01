//! The one place rig starts a process.

use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, IsTerminal, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Clone, Copy, Debug, Default)]
pub struct Policy {
    pub no_sudo: bool,
}

#[derive(Clone, Debug)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub sudo: bool,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
}

impl Cmd {
    #[must_use]
    pub fn new(program: &str, args: &[&str]) -> Cmd {
        Cmd {
            program: program.to_string(),
            args: args.iter().map(ToString::to_string).collect(),
            sudo: false,
            env: Vec::new(),
            cwd: None,
        }
    }

    #[must_use]
    pub fn with(mut self, extra: &[String]) -> Cmd {
        self.args.extend(extra.iter().cloned());
        self
    }

    #[must_use]
    pub fn as_root(mut self) -> Cmd {
        self.sudo = true;
        self
    }

    #[must_use]
    pub fn env(mut self, key: &str, value: &str) -> Cmd {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    #[must_use]
    pub fn in_dir(mut self, dir: PathBuf) -> Cmd {
        self.cwd = Some(dir);
        self
    }

    /// The command line as the user would type it.
    #[must_use]
    pub fn display(&self) -> String {
        let mut parts = Vec::new();
        if self.sudo && !is_root() {
            parts.push("sudo".to_string());
        }
        parts.push(self.program.clone());
        parts.extend(self.args.iter().cloned());
        parts.join(" ")
    }

    fn build(&self, policy: Policy) -> Result<Command> {
        let mut cmd = if !self.sudo || is_root() {
            let mut cmd = Command::new(&self.program);
            cmd.args(&self.args);
            for (k, v) in &self.env {
                cmd.env(k, v);
            }
            cmd
        } else {
            if policy.no_sudo {
                bail!("needs root (run in a terminal or with sudo)");
            }
            let mut cmd = Command::new("sudo");
            if batch() {
                cmd.arg("-n");
            }
            cmd.arg("--");
            // sudo resets the environment, so anything the command needs travels as argv.
            if !self.env.is_empty() {
                cmd.arg("env");
                for (k, v) in &self.env {
                    cmd.arg(format!("{k}={v}"));
                }
            }
            cmd.arg(&self.program).args(&self.args);
            cmd
        };
        if let Some(dir) = &self.cwd {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::null());
        Ok(cmd)
    }
}

/// Whether this process is already root.
#[must_use]
pub fn is_root() -> bool {
    // SAFETY-free: geteuid has no preconditions.
    let euid = unsafe { libc::geteuid() };
    euid == 0
}

fn batch() -> bool {
    !std::io::stdin().is_terminal()
}

/// Whether a passwordless sudo is available right now, checked once per process.
fn sudo_ready(policy: Policy) -> bool {
    static READY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *READY.get_or_init(|| {
        if policy.no_sudo || is_root() || !batch() {
            return true;
        }
        Command::new("sudo")
            .args(["-n", "--", "true"])
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// Whether `program` is on `PATH`.
#[must_use]
pub fn which(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|d| {
        std::fs::metadata(d.join(program))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

/// What a finished process left behind.
#[derive(Clone, Debug)]
pub struct Done {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

impl Done {
    #[must_use]
    pub fn ok(&self) -> bool {
        self.code == 0
    }

    /// The last lines of stderr, joined for a one-line note.
    #[must_use]
    pub fn reason(&self) -> String {
        let tail: Vec<&str> = self.stderr.lines().rev().take(20).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("; ")
    }
}

fn relay(reader: impl Read) -> String {
    let mut reader = BufReader::new(reader);
    let mut text = String::new();
    let mut buf = Vec::new();
    while reader.read_until(b'\n', &mut buf).is_ok_and(|n| n > 0) {
        let line = String::from_utf8_lossy(&buf);
        let line = line.trim_end_matches(['\n', '\r']);
        eprintln!("    {line}");
        text.push_str(line);
        text.push('\n');
        buf.clear();
    }
    text
}

/// Runs a command that changes the machine, echoing its output on stderr as it happens.
///
/// # Errors
/// When the process cannot be started.
pub fn try_run(cmd: &Cmd, policy: Policy) -> Result<Done> {
    if cmd.sudo && !sudo_ready(policy) {
        bail!("needs root (run in a terminal or with sudo)");
    }
    eprintln!("> {}", cmd.display());
    let mut child = cmd
        .build(policy)?
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running {}", cmd.program))?;
    let out = child.stdout.take().context("no stdout pipe")?;
    let err = child.stderr.take().context("no stderr pipe")?;
    let reader = std::thread::spawn(move || relay(out));
    let stderr = relay(err);
    let stdout = reader.join().unwrap_or_default();
    let status = child
        .wait()
        .with_context(|| format!("running {}", cmd.program))?;
    Ok(Done {
        stdout,
        stderr,
        code: status.code().unwrap_or(1),
    })
}

/// Like `try_run`, but a non-zero exit is an error.
///
/// # Errors
/// When the process cannot be started or exits non-zero.
pub fn run(cmd: &Cmd, policy: Policy) -> Result<Done> {
    let done = try_run(cmd, policy)?;
    if !done.ok() {
        bail!("{} failed with exit {}", cmd.program, done.code);
    }
    Ok(done)
}

/// Runs a query and returns its stdout. Queries are never echoed.
///
/// # Errors
/// When the process cannot be started or exits non-zero.
pub fn run_capture(cmd: &Cmd, policy: Policy) -> Result<String> {
    let out = cmd
        .build(policy)?
        .output()
        .with_context(|| format!("running {}", cmd.program))?;
    if !out.status.success() {
        bail!(
            "{} failed: {}",
            cmd.program,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs a query that is allowed to fail, quietly.
///
/// # Errors
/// When the process cannot be started at all.
pub fn query(cmd: &Cmd, policy: Policy) -> Result<Done> {
    let out = cmd
        .build(policy)?
        .output()
        .with_context(|| format!("running {}", cmd.program))?;
    Ok(Done {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(1),
    })
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Confirm {
    pub yes: bool,
}

impl Confirm {
    /// Asks on stderr and reads stdin. A non-tty stdin without `-y` answers no.
    #[must_use]
    pub fn ask(&self, question: &str) -> bool {
        if self.yes {
            return true;
        }
        if !std::io::stdin().is_terminal() {
            return false;
        }
        eprint!("{question} [y/N] ");
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
    }
}
