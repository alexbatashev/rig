//! The one place rig starts a process.

use anyhow::{bail, Context, Result};
use std::io::IsTerminal;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Output};

#[derive(Clone, Copy, Debug, Default)]
pub struct Policy {
    pub no_sudo: bool,
    pub verbose: bool,
}

#[derive(Clone, Debug)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
    pub sudo: bool,
    pub env: Vec<(String, String)>,
}

impl Cmd {
    #[must_use]
    pub fn new(program: &str, args: &[&str]) -> Cmd {
        Cmd {
            program: program.to_string(),
            args: args.iter().map(ToString::to_string).collect(),
            sudo: false,
            env: Vec::new(),
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

    fn build(&self, policy: Policy) -> Result<Command> {
        if !self.sudo || is_root() {
            let mut cmd = Command::new(&self.program);
            cmd.args(&self.args);
            for (k, v) in &self.env {
                cmd.env(k, v);
            }
            return Ok(cmd);
        }
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

/// Runs a command that changes the machine, echoing its output under `-v`.
///
/// # Errors
/// When the process cannot be started or exits non-zero.
pub fn run(cmd: &Cmd, policy: Policy) -> Result<Output> {
    if cmd.sudo && !sudo_ready(policy) {
        bail!("needs root (run in a terminal or with sudo)");
    }
    if policy.verbose {
        // Live output, so a long install does not look hung.
        let status = cmd
            .build(policy)?
            .status()
            .with_context(|| format!("running {}", cmd.program))?;
        if !status.success() {
            bail!(
                "{} failed with exit {}",
                cmd.program,
                status.code().unwrap_or(1)
            );
        }
        return Ok(Output {
            status,
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
    }
    let out = capture(cmd, policy)?;
    if !out.status.success() {
        let tail = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = tail.lines().rev().take(20).collect();
        bail!(
            "{} failed: {}",
            cmd.program,
            tail.into_iter().rev().collect::<Vec<_>>().join("; ")
        );
    }
    Ok(out)
}

fn capture(cmd: &Cmd, policy: Policy) -> Result<Output> {
    cmd.build(policy)?
        .output()
        .with_context(|| format!("running {}", cmd.program))
}

/// Runs a query and returns its stdout. Queries are never echoed, however verbose the run.
///
/// # Errors
/// When the process cannot be started or exits non-zero.
pub fn run_capture(cmd: &Cmd, policy: Policy) -> Result<String> {
    let out = capture(cmd, policy)?;
    if !out.status.success() {
        bail!(
            "{} failed: {}",
            cmd.program,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs a command that is allowed to fail, returning `(stdout, stderr, ok)`.
///
/// # Errors
/// When the process cannot be started at all.
pub fn try_run(cmd: &Cmd, policy: Policy) -> Result<(String, String, bool)> {
    let out = cmd
        .build(policy)?
        .output()
        .with_context(|| format!("running {}", cmd.program))?;
    Ok((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    ))
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
