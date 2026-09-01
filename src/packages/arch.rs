use super::{names_of, NotFound, PackageBackend};
use crate::exec::{query, run, run_capture, try_run, which, Cmd, Policy};
use crate::repo::Backend;
use anyhow::{bail, Result};
use std::collections::BTreeSet;

pub struct Arch;

impl Arch {
    fn aur_helper() -> Option<&'static str> {
        ["yay", "paru"].into_iter().find(|h| which(h))
    }
}

impl PackageBackend for Arch {
    fn kind(&self) -> Backend {
        Backend::Arch
    }

    fn available(&self) -> bool {
        which("pacman")
    }

    fn version(&self, policy: Policy) -> Option<String> {
        // pacman -V leads with ASCII art; the version sits inside one of those lines.
        run_capture(&Cmd::new("pacman", &["-V"]), policy)
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.find("Pacman v").map(|i| l[i..].trim().to_string()))
            })
    }

    fn installed(&self, names: &BTreeSet<String>, policy: Policy) -> Result<BTreeSet<String>> {
        let out = run_capture(&Cmd::new("pacman", &["-Qq"]), policy)?;
        Ok(names_of(&out, names))
    }

    fn install(&mut self, names: &[String], policy: Policy) -> Result<()> {
        let probe = query(
            &Cmd::new("pacman", &["-Sp", "--print-format", "%n"]).with(names),
            policy,
        )?;
        let aur: Vec<String> = probe
            .stderr
            .lines()
            .filter_map(|l| l.trim().strip_prefix("error: target not found: "))
            .map(ToString::to_string)
            .collect();
        let repo: Vec<String> = names.iter().filter(|n| !aur.contains(n)).cloned().collect();
        if !repo.is_empty() {
            run(
                &Cmd::new("pacman", &["-S", "--needed", "--noconfirm"])
                    .with(&repo)
                    .as_root(),
                policy,
            )?;
        }
        if !aur.is_empty() {
            let Some(helper) = Arch::aur_helper() else {
                bail!("no AUR helper (yay or paru) for: {}", aur.join(", "));
            };
            for name in &aur {
                let done = try_run(
                    &Cmd::new(helper, &["-S", "--needed", "--noconfirm", name]),
                    policy,
                )?;
                if done.ok() {
                    continue;
                }
                if done.stderr.contains("target not found")
                    || done.stderr.contains("Could not find all required packages")
                {
                    return Err(NotFound(name.clone()).into());
                }
                bail!("{helper} -S {name} failed with exit {}", done.code);
            }
        }
        Ok(())
    }

    fn remove(&mut self, names: &[String], policy: Policy) -> Result<()> {
        run(
            &Cmd::new("pacman", &["-Rns", "--noconfirm"])
                .with(names)
                .as_root(),
            policy,
        )?;
        Ok(())
    }
}
