use super::{names_of, NotFound, PackageBackend};
use crate::exec::{run, run_capture, try_run, which, Cmd, Policy};
use crate::repo::Backend;
use anyhow::Result;
use std::collections::BTreeSet;

pub struct Apt;

impl PackageBackend for Apt {
    fn kind(&self) -> Backend {
        Backend::Ubuntu
    }

    fn available(&self) -> bool {
        which("apt-get")
    }

    fn version(&self, policy: Policy) -> Option<String> {
        run_capture(&Cmd::new("apt-get", &["-v"]), policy)
            .ok()
            .and_then(|s| s.lines().next().map(ToString::to_string))
    }

    fn installed(&self, names: &BTreeSet<String>, policy: Policy) -> Result<BTreeSet<String>> {
        let out = run_capture(
            &Cmd::new(
                "dpkg-query",
                &["-W", "-f=${binary:Package}\t${db:Status-Status}\n"],
            ),
            policy,
        )?;
        let listed = out
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .filter(|(_, status)| status.trim() == "installed")
            .map(|(name, _)| name.split(':').next().unwrap_or(name))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(names_of(&listed, names))
    }

    fn install(&mut self, names: &[String], policy: Policy) -> Result<()> {
        for name in names {
            let done = try_run(
                &Cmd::new(
                    "apt-get",
                    &["install", "-y", "--no-install-recommends", name],
                )
                .env("DEBIAN_FRONTEND", "noninteractive")
                .as_root(),
                policy,
            )?;
            if done.ok() {
                continue;
            }
            if done.stderr.contains("Unable to locate package") {
                return Err(NotFound(name.clone()).into());
            }
            anyhow::bail!("apt-get install {name} failed: {}", done.reason());
        }
        Ok(())
    }

    fn remove(&mut self, names: &[String], policy: Policy) -> Result<()> {
        run(
            &Cmd::new("apt-get", &["remove", "-y"])
                .with(names)
                .env("DEBIAN_FRONTEND", "noninteractive")
                .as_root(),
            policy,
        )?;
        Ok(())
    }
}
