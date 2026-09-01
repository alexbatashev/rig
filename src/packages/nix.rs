use super::{NotFound, PackageBackend};
use crate::exec::{query, run, run_capture, try_run, which, Cmd, Policy};
use crate::repo::Backend;
use anyhow::Result;
use std::collections::BTreeSet;

pub struct Nix;

/// `nixpkgs#python3Packages.black` is tracked as `black`.
fn short_name(flake_ref: &str) -> String {
    flake_ref
        .rsplit_once('#')
        .map_or(flake_ref, |(_, attr)| attr)
        .rsplit('.')
        .next()
        .unwrap_or(flake_ref)
        .to_string()
}

fn element_names(json: &str) -> BTreeSet<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return BTreeSet::new();
    };
    match &v["elements"] {
        serde_json::Value::Object(map) => map.keys().cloned().collect(),
        serde_json::Value::Array(list) => list
            .iter()
            .filter_map(|e| {
                ["attrPath", "originalUrl", "url"]
                    .iter()
                    .filter_map(|k| e[*k].as_str())
                    .find(|v| v.contains('#'))
                    .map(short_name)
            })
            .collect(),
        _ => BTreeSet::new(),
    }
}

impl PackageBackend for Nix {
    fn kind(&self) -> Backend {
        Backend::Nix
    }

    fn available(&self) -> bool {
        which("nix")
            && query(&Cmd::new("nix", &["profile", "list"]), Policy::default())
                .is_ok_and(|d| d.ok())
    }

    fn version(&self, policy: Policy) -> Option<String> {
        run_capture(&Cmd::new("nix", &["--version"]), policy)
            .ok()
            .and_then(|s| s.lines().next().map(ToString::to_string))
    }

    fn installed(&self, names: &BTreeSet<String>, policy: Policy) -> Result<BTreeSet<String>> {
        let out = run_capture(&Cmd::new("nix", &["profile", "list", "--json"]), policy)?;
        let present = element_names(&out);
        Ok(names
            .iter()
            .filter(|n| present.contains(&short_name(n)))
            .cloned()
            .collect())
    }

    fn install(&mut self, names: &[String], policy: Policy) -> Result<()> {
        for name in names {
            let done = try_run(&Cmd::new("nix", &["profile", "install", name]), policy)?;
            if done.ok() {
                continue;
            }
            if done.stderr.contains("does not provide attribute") {
                return Err(NotFound(name.clone()).into());
            }
            anyhow::bail!("nix profile install {name} failed with exit {}", done.code);
        }
        Ok(())
    }

    fn remove(&mut self, names: &[String], policy: Policy) -> Result<()> {
        let short: Vec<String> = names.iter().map(|n| short_name(n)).collect();
        let done = try_run(
            &Cmd::new("nix", &["profile", "remove"]).with(&short),
            policy,
        )?;
        if done.ok() {
            return Ok(());
        }
        if !done.stderr.contains("unknown element") {
            anyhow::bail!("nix profile remove failed with exit {}", done.code);
        }
        for name in &short {
            run(
                &Cmd::new(
                    "nix",
                    &["profile", "remove", "--regex", &format!("^{name}$")],
                ),
                policy,
            )?;
        }
        Ok(())
    }
}
