//! Command line entry points.

use crate::compose::{desired, Desired};
use crate::repo::{load, select, Os, Repo, KNOWN_OS_TAGS};
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "rig",
    version,
    about = "Compose, apply and absorb config files"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report repo parse errors and suspicious variant tags.
    Doctor {
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
    /// Print the composed content of every managed target.
    #[command(hide = true)]
    Compose {
        #[arg(long)]
        host: String,
        /// `RIG_OS` form: `linux:arch`, `linux:omarchy:arch` or `macos`.
        #[arg(long)]
        os: String,
        /// Write the composed tree here instead of printing it.
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(default_value = ".")]
        repo: PathBuf,
    },
}

/// Runs the command named on the command line and returns its exit code.
///
/// # Errors
/// On any failure that is not reported as a row.
pub fn run(cli: Cli) -> Result<i32> {
    match cli.command {
        Command::Doctor { repo } => Ok(doctor(&repo)),
        Command::Compose {
            host,
            os,
            out,
            repo,
        } => compose_cmd(&repo, &host, &Os::parse(&os), out.as_deref()),
    }
}

fn doctor(root: &Path) -> i32 {
    let repo = match load(root) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e:#}");
            return 2;
        }
    };
    let mut warnings = Vec::new();
    for d in &repo.missing_defaults {
        warnings.push(format!("defaults dir not present: {}", d.display()));
    }
    warnings.extend(unknown_variant_tags(&repo));
    warnings.extend(ini_case_collisions(&repo));
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    println!(
        "{} modules, {} hosts, {} defaults dirs",
        repo.modules.len(),
        repo.hosts.len(),
        repo.defaults.len()
    );
    i32::from(!warnings.is_empty())
}

fn unknown_variant_tags(repo: &Repo) -> Vec<String> {
    let mut known: BTreeSet<&str> = KNOWN_OS_TAGS.iter().copied().collect();
    known.extend(repo.hosts.keys().map(String::as_str));
    for m in repo.modules.values() {
        if let Some(tags) = &m.when_os {
            known.extend(tags.iter().map(String::as_str));
        }
    }
    repo.modules
        .values()
        .flat_map(|m| &m.files)
        .chain(repo.defaults.iter().flat_map(|d| &d.files))
        .filter_map(|f| {
            f.variant
                .as_deref()
                .filter(|t| !known.contains(t))
                .map(|t| {
                    format!(
                        "{}: variant '{t}' matches no host and no known OS",
                        f.path.display()
                    )
                })
        })
        .collect()
}

fn ini_case_collisions(repo: &Repo) -> Vec<String> {
    let mut out = Vec::new();
    for f in repo.modules.values().flat_map(|m| &m.files) {
        let Ok(text) = std::fs::read_to_string(&f.path) else {
            continue;
        };
        let Ok(parsed) = crate::ini::parse(&text) else {
            continue;
        };
        for (a, b) in crate::ini::case_collisions(&parsed) {
            out.push(format!(
                "{}: sections [{a}] and [{b}] differ only by case; git treats them as one",
                f.path.display()
            ));
        }
    }
    out
}

fn compose_cmd(root: &Path, host: &str, os: &Os, out: Option<&Path>) -> Result<i32> {
    let repo = load(root)?;
    let sel = select(&repo, host, os)?;
    let items = desired(&repo, &sel, os)?;
    match out {
        Some(dir) => write_tree(&items, dir)?,
        None => {
            for d in &items {
                println!(
                    "=== {} ({:?}, {:o}, {})",
                    d.target, d.format, d.mode, d.module
                );
                print!("{}", String::from_utf8_lossy(&d.content));
            }
        }
    }
    Ok(0)
}

fn write_tree(items: &[Desired], dir: &Path) -> Result<()> {
    for d in items {
        let path = dir.join(d.target.module_path());
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, &d.content)?;
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(d.mode))?;
    }
    Ok(())
}
