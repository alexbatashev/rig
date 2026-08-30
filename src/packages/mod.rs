//! Installing and removing what the active modules list.

pub mod apt;
pub mod arch;
pub mod brew;
pub mod keel;
pub mod mise;
pub mod nix;

use crate::exec::{Confirm, Policy};
use crate::repo::{Backend, Os, Selection};
use crate::report::{Report, Row};
use crate::state::{State, Store};
use anyhow::Result;
use std::collections::BTreeSet;
use std::fmt::Write as _;

pub trait PackageBackend {
    fn kind(&self) -> Backend;
    /// Whether the tool this backend drives is on this machine.
    fn available(&self) -> bool;
    fn version(&self, policy: Policy) -> Option<String>;
    /// Told the full desired set before anything is queried, for backends that
    /// keep a config file of their own.
    ///
    /// # Errors
    /// When that file cannot be written.
    fn desired(&mut self, _want: &BTreeSet<String>) -> Result<()> {
        Ok(())
    }
    /// The subset of `names` installed right now.
    ///
    /// # Errors
    /// When the package manager cannot be queried.
    fn installed(&self, names: &BTreeSet<String>, policy: Policy) -> Result<BTreeSet<String>>;
    /// # Errors
    /// When installation fails.
    fn install(&mut self, names: &[String], policy: Policy) -> Result<()>;
    /// # Errors
    /// When removal fails.
    fn remove(&mut self, names: &[String], policy: Policy) -> Result<()>;
    /// Names this backend installed as casks, for `state.macos_casks`.
    fn casks(&self) -> Option<BTreeSet<String>> {
        None
    }
}

/// The lines of `listing` that name something in `wanted`.
#[must_use]
pub fn names_of(listing: &str, wanted: &BTreeSet<String>) -> BTreeSet<String> {
    listing
        .lines()
        .map(str::trim)
        .filter(|l| wanted.contains(*l))
        .map(ToString::to_string)
        .collect()
}

/// A package manager's "no such package" answer, so `step` can point at `packages.toml`.
#[derive(Debug)]
pub struct NotFound(pub String);

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: not found", self.0)
    }
}

impl std::error::Error for NotFound {}

/// The native backend for this OS, then nix and mise everywhere.
#[must_use]
pub fn backends_for(os: &Os, casks: BTreeSet<String>) -> Vec<Box<dyn PackageBackend>> {
    let mut out: Vec<Box<dyn PackageBackend>> = match Backend::native(os) {
        Some(Backend::Arch) => vec![Box::new(arch::Arch)],
        Some(Backend::Ubuntu) => vec![Box::new(apt::Apt)],
        Some(Backend::Macos) => vec![Box::new(brew::Brew::new(casks))],
        Some(Backend::Keel) => vec![Box::new(keel::Keel)],
        _ => Vec::new(),
    };
    out.push(Box::new(nix::Nix));
    out.push(Box::new(mise::Mise));
    out
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub confirm: Confirm,
    pub policy: Policy,
    pub dry_run: bool,
}

fn wanted(sel: &Selection, backend: Backend) -> BTreeSet<String> {
    sel.packages.get(&backend).cloned().unwrap_or_default()
}

/// Brings every backend in line with the active modules, recording what rig installed.
///
/// # Errors
/// When the state cannot be saved.
pub fn sync(
    sel: &Selection,
    os: &Os,
    state: &mut State,
    store: &Store,
    opts: Options,
) -> Result<Report> {
    let mut report = Report::default();
    for name in &sel.absent {
        report.push(
            Row::new("package", name)
                .module(&format!(
                    "not on {}",
                    Backend::native(os).map_or("this os".to_string(), |b| b.to_string())
                ))
                .quietly(),
        );
    }
    let mut backends = backends_for(os, state.macos_casks.clone());
    for backend in &mut backends {
        let kind = backend.kind();
        let want = wanted(sel, kind);
        let tracked = state.packages.get(&kind).cloned().unwrap_or_default();
        if want.is_empty() && tracked.is_empty() {
            continue;
        }
        if !backend.available() {
            report.push(
                Row::new("skipped", &format!("packages: {kind}"))
                    .module(&format!("{kind} tool not on PATH")),
            );
            continue;
        }
        step(backend.as_mut(), &want, tracked, state, opts, &mut report);
        if let Some(casks) = backend.casks() {
            state.macos_casks = casks;
        }
        store.save(state)?;
    }
    Ok(report)
}

fn error_row(kind: Backend, e: &anyhow::Error) -> Row {
    if let Some(NotFound(name)) = e.downcast_ref::<NotFound>() {
        return Row::new("error", &format!("package {name}"))
            .note(&format!(
                "not found on {kind} (add [{name}] {kind} = \"...\" to packages.toml)"
            ))
            .exit(2);
    }
    Row::new("error", &format!("packages: {kind}"))
        .note(&format!("{e:#}"))
        .exit(2)
}

fn step(
    backend: &mut dyn PackageBackend,
    want: &BTreeSet<String>,
    mut tracked: BTreeSet<String>,
    state: &mut State,
    opts: Options,
    report: &mut Report,
) {
    let kind = backend.kind();
    let query: BTreeSet<String> = want.union(&tracked).cloned().collect();
    let present = match backend.installed(&query, opts.policy) {
        Ok(p) => p,
        Err(e) => {
            report.push(error_row(kind, &e));
            return;
        }
    };
    let prepare = |backend: &mut dyn PackageBackend, report: &mut Report| {
        if opts.dry_run {
            return true;
        }
        match backend.desired(want) {
            Ok(()) => true,
            Err(e) => {
                report.push(error_row(kind, &e));
                false
            }
        }
    };
    let to_add: Vec<String> = want.difference(&present).cloned().collect();
    let unwanted: BTreeSet<String> = tracked.difference(want).cloned().collect();
    // Something rig installed that is already gone just leaves the ledger.
    for name in unwanted.difference(&present) {
        tracked.remove(name);
    }
    let to_remove: Vec<String> = unwanted.intersection(&present).cloned().collect();

    if to_add.is_empty() && to_remove.is_empty() {
        if !prepare(backend, report) {
            return;
        }
        let mut row = Row::new("package", &format!("{kind}: {} ok", want.len()));
        row.quiet = true;
        report.push(row);
        state.packages.insert(kind, tracked);
        return;
    }
    if opts.dry_run {
        for n in &to_add {
            report.push(Row::new("package", &format!("{kind}: {n}")).module("would install"));
        }
        for n in &to_remove {
            report.push(Row::new("package", &format!("{kind}: {n}")).module("would remove"));
        }
        return;
    }

    let mut question = format!("{kind}:");
    if !to_add.is_empty() {
        let _ = write!(question, " install {}", to_add.join(", "));
    }
    if !to_remove.is_empty() {
        let sep = if to_add.is_empty() { "" } else { ";" };
        let _ = write!(question, "{sep} remove {}", to_remove.join(", "));
    }
    if !opts.confirm.ask(&format!("{question}?")) {
        report.push(
            Row::new("skipped", &format!("packages: {kind}"))
                .module("declined")
                .exit(1),
        );
        return;
    }
    if !prepare(backend, report) {
        return;
    }

    if !to_add.is_empty() {
        if let Err(e) = backend.install(&to_add, opts.policy) {
            // Some of the batch may still have landed; only track what is really there.
            let landed = backend
                .installed(&to_add.iter().cloned().collect(), opts.policy)
                .unwrap_or_default();
            for n in &landed {
                report.push(Row::new("package", &format!("{kind}: {n}")).module("installed"));
            }
            tracked.extend(landed);
            report.push(error_row(kind, &e));
            state.packages.insert(kind, tracked);
            return;
        }
        tracked.extend(to_add.iter().cloned());
        for n in &to_add {
            report.push(Row::new("package", &format!("{kind}: {n}")).module("installed"));
        }
    }
    if !to_remove.is_empty() {
        match backend.remove(&to_remove, opts.policy) {
            Ok(()) => {
                for n in &to_remove {
                    tracked.remove(n);
                    report.push(Row::new("package", &format!("{kind}: {n}")).module("removed"));
                }
            }
            Err(e) => report.push(error_row(kind, &e)),
        }
    }
    state.packages.insert(kind, tracked);
}

/// One `doctor` line per backend rig knows about, applicable to this OS or not.
#[must_use]
pub fn doctor_lines(policy: Policy) -> Vec<String> {
    let all: Vec<Box<dyn PackageBackend>> = vec![
        Box::new(arch::Arch),
        Box::new(apt::Apt),
        Box::new(brew::Brew::new(BTreeSet::new())),
        Box::new(nix::Nix),
        Box::new(mise::Mise),
    ];
    all.iter()
        .map(|b| {
            let kind = b.kind();
            if b.available() {
                let v = b.version(policy).unwrap_or_else(|| "available".to_string());
                format!("{kind}: {v}")
            } else {
                format!("{kind}: not available")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backends_are_native_then_nix_then_mise() {
        let kinds = |spec: &str| -> Vec<Backend> {
            backends_for(&Os::parse(spec), BTreeSet::new())
                .iter()
                .map(|b| b.kind())
                .collect()
        };
        assert_eq!(
            kinds("linux:arch"),
            vec![Backend::Arch, Backend::Nix, Backend::Mise]
        );
        assert_eq!(
            kinds("linux:ubuntu"),
            vec![Backend::Ubuntu, Backend::Nix, Backend::Mise]
        );
        assert_eq!(
            kinds("macos"),
            vec![Backend::Macos, Backend::Nix, Backend::Mise]
        );
        assert_eq!(
            kinds("linux:keel"),
            vec![Backend::Keel, Backend::Nix, Backend::Mise]
        );
        assert_eq!(kinds("linux:void"), vec![Backend::Nix, Backend::Mise]);
    }

    #[test]
    fn names_of_intersects_a_listing() {
        let wanted: BTreeSet<String> = ["a".into(), "b".into()].into();
        assert_eq!(
            names_of("a\nc\n b \n", &wanted),
            ["a".to_string(), "b".to_string()].into()
        );
    }
}
