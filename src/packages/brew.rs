use super::{names_of, PackageBackend};
use crate::exec::{run, run_capture, try_run, which, Cmd, Policy};
use crate::repo::Backend;
use anyhow::Result;
use std::collections::BTreeSet;

pub struct Brew {
    /// Names rig installed as casks, so removal passes `--cask`.
    pub casks: BTreeSet<String>,
}

impl Brew {
    #[must_use]
    pub fn new(casks: BTreeSet<String>) -> Brew {
        Brew { casks }
    }
}

fn missing_formula(stderr: &str) -> bool {
    stderr.contains("No available formula") || stderr.contains("No formulae or casks found")
}

impl PackageBackend for Brew {
    fn kind(&self) -> Backend {
        Backend::Macos
    }

    fn available(&self) -> bool {
        which("brew")
    }

    fn version(&self, policy: Policy) -> Option<String> {
        run_capture(&Cmd::new("brew", &["--version"]), policy)
            .ok()
            .and_then(|s| s.lines().next().map(ToString::to_string))
    }

    fn installed(&self, names: &BTreeSet<String>, policy: Policy) -> Result<BTreeSet<String>> {
        let mut listed = run_capture(&Cmd::new("brew", &["list", "-1", "--formula"]), policy)?;
        listed.push_str(&run_capture(
            &Cmd::new("brew", &["list", "-1", "--cask"]),
            policy,
        )?);
        Ok(names_of(&listed, names))
    }

    fn install(&mut self, names: &[String], policy: Policy) -> Result<()> {
        for name in names {
            let (_, stderr, ok) = try_run(&Cmd::new("brew", &["install", name]), policy)?;
            if ok {
                continue;
            }
            if !missing_formula(&stderr) {
                anyhow::bail!("brew install {name} failed: {}", stderr.trim());
            }
            run(&Cmd::new("brew", &["install", "--cask", name]), policy)?;
            self.casks.insert(name.clone());
        }
        Ok(())
    }

    fn remove(&mut self, names: &[String], policy: Policy) -> Result<()> {
        for name in names {
            let cmd = if self.casks.contains(name) {
                Cmd::new("brew", &["uninstall", "--cask", name])
            } else {
                Cmd::new("brew", &["uninstall", name])
            };
            run(&cmd, policy)?;
            self.casks.remove(name);
        }
        Ok(())
    }

    fn casks(&self) -> Option<BTreeSet<String>> {
        Some(self.casks.clone())
    }
}
