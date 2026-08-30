use super::PackageBackend;
use crate::exec::{run, run_capture, which, Cmd, Policy};
use crate::repo::Backend;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

pub struct Mise;

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    match std::env::var_os(var) {
        Some(d) => Some(PathBuf::from(d)),
        None => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(fallback)),
    }
}

fn config_path() -> Option<PathBuf> {
    xdg("XDG_CONFIG_HOME", ".config").map(|d| d.join("mise/conf.d/rig.toml"))
}

fn toml_key(name: &str) -> String {
    if name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        name.to_string()
    } else {
        format!("\"{name}\"")
    }
}

fn render(want: &BTreeSet<String>) -> String {
    let mut out = String::from("# written by rig, edit module.toml instead\n[tools]\n");
    for name in want {
        let _ = writeln!(out, "{} = \"latest\"", toml_key(name));
    }
    out
}

/// Tools `mise ls --json` reports as installed.
fn installed_names(json: &str) -> BTreeSet<String> {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(json) else {
        return BTreeSet::new();
    };
    map.into_iter()
        .filter(|(_, entries)| {
            entries
                .as_array()
                .is_some_and(|l| l.iter().any(|e| e["installed"].as_bool() == Some(true)))
        })
        .map(|(k, _)| k)
        .collect()
}

/// A `doctor` warning when mise tools would not be on `PATH` in a shell.
#[must_use]
pub fn path_warning() -> Option<String> {
    if !which("mise") || std::env::var_os("MISE_SHELL").is_some() {
        return None;
    }
    let shims = std::env::var_os("MISE_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| xdg("XDG_DATA_HOME", ".local/share").map(|d| d.join("mise")))?
        .join("shims");
    let on_path =
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == shims));
    (!on_path)
        .then(|| "mise: tools not on PATH (add mise activate to your shell config)".to_string())
}

impl PackageBackend for Mise {
    fn kind(&self) -> Backend {
        Backend::Mise
    }

    fn available(&self) -> bool {
        which("mise")
    }

    fn version(&self, policy: Policy) -> Option<String> {
        run_capture(&Cmd::new("mise", &["--version"]), policy)
            .ok()
            .and_then(|s| s.lines().next().map(ToString::to_string))
    }

    fn desired(&mut self, want: &BTreeSet<String>) -> Result<()> {
        let path = config_path().context("HOME is not set")?;
        if want.is_empty() {
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
            return Ok(());
        }
        let body = render(want);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(body.as_str()) {
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))
    }

    fn installed(&self, names: &BTreeSet<String>, policy: Policy) -> Result<BTreeSet<String>> {
        let out = run_capture(&Cmd::new("mise", &["ls", "--json", "--installed"]), policy)?;
        Ok(installed_names(&out).intersection(names).cloned().collect())
    }

    fn install(&mut self, _names: &[String], policy: Policy) -> Result<()> {
        run(&Cmd::new("mise", &["install"]).env("MISE_YES", "1"), policy)?;
        Ok(())
    }

    fn remove(&mut self, names: &[String], policy: Policy) -> Result<()> {
        for name in names {
            run(&Cmd::new("mise", &["uninstall", name]), policy)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_quoted_keys() {
        let want: BTreeSet<String> = ["gh".into(), "npm:@anthropic-ai/claude-code".into()].into();
        assert_eq!(
            render(&want),
            "# written by rig, edit module.toml instead\n[tools]\ngh = \"latest\"\n\"npm:@anthropic-ai/claude-code\" = \"latest\"\n"
        );
    }

    #[test]
    fn parses_installed_flag() {
        let json = r#"{"gh":[{"installed":true}],"node":[{"installed":false}],"x":[]}"#;
        assert_eq!(installed_names(json), ["gh".to_string()].into());
    }
}
