#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

pub struct Sandbox {
    pub root: TempDir,
    pub home: PathBuf,
    pub repo: PathBuf,
    pub os: String,
    env: std::collections::BTreeMap<String, String>,
}

/// The only system programs a sandboxed run may reach. Everything else must be faked,
/// so a test can never invoke the real package manager.
const SYSTEM_TOOLS: &[&str] = &[
    "sh", "bash", "env", "printf", "basename", "dirname", "cat", "grep", "sed", "mv", "cp", "rm",
    "ls", "head", "tail", "sort", "wc", "chmod", "mkdir", "true", "false", "uname", "git",
];

fn link_system_tools(into: &Path) {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for tool in SYSTEM_TOOLS {
        let found = std::env::split_paths(&path)
            .map(|d| d.join(tool))
            .find(|p| p.is_file())
            .unwrap_or_else(|| panic!("the sandbox needs {tool} on PATH"));
        let _ = std::os::unix::fs::symlink(found, into.join(tool));
    }
}

fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::remove_file(path);
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, PermissionsExt::from_mode(0o755)).unwrap();
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), &dst).unwrap();
            let mode = std::fs::metadata(e.path()).unwrap().permissions();
            std::fs::set_permissions(&dst, mode).unwrap();
        }
    }
}

#[must_use]
pub fn fixture_dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

impl Sandbox {
    #[must_use]
    pub fn new() -> Sandbox {
        let root = TempDir::new().unwrap();
        let home = root.path().join("home");
        let repo = root.path().join("repo");
        for d in [
            &home,
            &repo,
            &root.path().join("bin"),
            &root.path().join("sysbin"),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        link_system_tools(&root.path().join("sysbin"));
        // The basic fixture's hypr module runs `hyprctl reload`; a test that cares
        // about it puts its own fake in bin/, which comes first on PATH.
        write_script(&root.path().join("sysbin/hyprctl"), "#!/bin/sh\nexit 0\n");
        Sandbox {
            root,
            home,
            repo,
            os: "linux:arch".to_string(),
            env: std::collections::BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_fixture(name: &str) -> Sandbox {
        let sb = Sandbox::new();
        copy_dir(&fixture_dir(name), &sb.repo);
        sb
    }

    #[must_use]
    pub fn with_os(mut self, os: &str) -> Sandbox {
        self.os = os.to_string();
        self
    }

    pub fn write_home(&self, rel: &str, content: &str) {
        let p = self.home.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[must_use]
    pub fn read_home(&self, rel: &str) -> String {
        std::fs::read_to_string(self.home.join(rel)).unwrap()
    }

    #[must_use]
    pub fn home_exists(&self, rel: &str) -> bool {
        self.home.join(rel).exists()
    }

    #[must_use]
    pub fn mode(&self, rel: &str) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(self.home.join(rel))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    }

    pub fn write_repo(&self, rel: &str, content: &str) {
        let p = self.repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.root.path().join("state/rig")
    }

    #[must_use]
    pub fn etc_root(&self) -> PathBuf {
        self.root.path().join("etcroot")
    }

    #[must_use]
    pub fn log_path(&self) -> PathBuf {
        self.root.path().join("fake.log")
    }

    #[must_use]
    pub fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.log_path())
            .unwrap_or_default()
            .lines()
            .map(ToString::to_string)
            .collect()
    }

    pub fn set_env(&mut self, key: &str, value: &str) {
        self.env.insert(key.to_string(), value.to_string());
    }

    fn write_list(&self, name: &str, lines: &[&str]) {
        let body = lines
            .iter()
            .map(|l| format!("{l}\n"))
            .collect::<Vec<_>>()
            .concat();
        std::fs::write(self.root.path().join(name), body).unwrap();
    }

    /// Packages the fake package managers report as already installed.
    pub fn set_installed(&self, names: &[&str]) {
        self.write_list("fake.installed", names);
    }

    /// Packages the fake `pacman -Sp` recognises; everything else is AUR.
    pub fn set_repo_packages(&self, names: &[&str]) {
        self.write_list("fake.repo", names);
    }

    /// Names the fake `brew install` rejects as a formula.
    pub fn set_casks(&self, names: &[&str]) {
        self.write_list("fake.casks", names);
    }

    pub fn set_nix_json(&self, json: &str) {
        std::fs::write(self.root.path().join("fake.nix.json"), json).unwrap();
    }

    /// What the fake `mise ls --json` answers.
    pub fn set_mise_json(&self, json: &str) {
        std::fs::write(self.root.path().join("fake.mise.json"), json).unwrap();
    }

    pub fn remove_fake_bin(&self, name: &str) {
        let _ = std::fs::remove_file(self.root.path().join("bin").join(name));
    }

    /// Writes an executable script into the sandbox `bin`, which is first on `PATH`.
    pub fn fake_bin(&self, name: &str, script: &str) {
        write_script(&self.root.path().join("bin").join(name), script);
    }

    pub fn rig(&self, args: &[&str]) -> Run {
        let path = format!(
            "{}:{}",
            self.root.path().join("bin").display(),
            self.root.path().join("sysbin").display()
        );
        let out = Command::new(env!("CARGO_BIN_EXE_rig"))
            .args(args)
            .current_dir(self.root.path())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env("XDG_STATE_HOME", self.root.path().join("state"))
            .env("RIG_OS", &self.os)
            .env("PATH", path)
            .env("FAKE_LOG", self.log_path())
            .env("FAKE_INSTALLED", self.root.path().join("fake.installed"))
            .env("FAKE_REPO", self.root.path().join("fake.repo"))
            .env("FAKE_CASKS", self.root.path().join("fake.casks"))
            .env("FAKE_NIX_JSON", self.root.path().join("fake.nix.json"))
            .env("FAKE_MISE_JSON", self.root.path().join("fake.mise.json"))
            .envs(self.env.clone())
            .env_remove("MISE_SHELL")
            .env_remove("MISE_DATA_DIR")
            .env_remove("EDITOR")
            .env_remove("VISUAL")
            .output()
            .unwrap();
        Run {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

pub struct Run {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Run {
    /// `(outcome, target)` for every summary row.
    #[must_use]
    pub fn rows(&self) -> Vec<(String, String)> {
        self.stdout
            .lines()
            .filter(|l| l.starts_with("  "))
            .filter_map(|l| {
                let mut it = l.split_whitespace();
                Some((it.next()?.to_string(), it.next()?.to_string()))
            })
            .collect()
    }

    #[must_use]
    pub fn outcome(&self, target: &str) -> Option<String> {
        self.rows()
            .into_iter()
            .find(|(_, t)| t == target)
            .map(|(o, _)| o)
    }
}
