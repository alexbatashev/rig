//! System settings rig reconciles the way it does files: read, compare, write what differs.

pub mod macos;

use crate::exec::Policy;
use crate::repo::Selection;
use crate::report::{Report, Row};
use crate::state::State;
use anyhow::Result;
use std::collections::BTreeMap;

pub trait Provider {
    fn name(&self) -> &'static str;
    fn available(&self) -> bool;
    /// The machine's value, `None` when the key is unset.
    ///
    /// # Errors
    /// When the settings tool cannot be queried.
    fn read(&self, domain: &str, key: &str, policy: Policy) -> Result<Option<String>>;
    /// # Errors
    /// When the write fails.
    fn write(&self, domain: &str, key: &str, value: &toml::Value, policy: Policy) -> Result<()>;
    /// What `read` returns once `value` is written.
    fn normalise(&self, value: &toml::Value) -> String;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub policy: Policy,
    pub dry_run: bool,
    pub force: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Adopted,
    Set,
    Ok,
    Updated,
    Edited,
}

/// One declared setting against the machine and the ledger.
#[derive(Clone, Debug)]
pub struct Item {
    pub provider: &'static str,
    pub domain: String,
    pub key: String,
    pub value: toml::Value,
    pub desired: String,
    pub machine: Option<String>,
    pub outcome: Outcome,
}

impl Item {
    /// `domain.key`, how rows and `rig diff` name a setting.
    #[must_use]
    pub fn label(&self) -> String {
        format!("{}.{}", self.domain, self.key)
    }

    fn state_key(&self) -> String {
        format!("{}:{}", self.provider, self.label())
    }
}

fn same(a: &str, b: &str) -> bool {
    a == b
        || matches!((a.parse::<f64>(), b.parse::<f64>()), (Ok(x), Ok(y)) if (x - y).abs() < f64::EPSILON)
}

fn outcome(ledger: Option<&str>, machine: Option<&str>, desired: &str) -> Outcome {
    match (ledger, machine) {
        (None, Some(k)) if same(k, desired) => Outcome::Adopted,
        (None, _) => Outcome::Set,
        (Some(l), Some(k)) if same(k, l) => {
            if same(desired, l) {
                Outcome::Ok
            } else {
                Outcome::Updated
            }
        }
        (Some(_), _) => Outcome::Edited,
    }
}

/// Reads every declared setting. Providers a module needs but the machine lacks are skipped.
///
/// # Errors
/// When a provider cannot be queried.
pub fn plan(
    providers: &[Box<dyn Provider>],
    sel: &Selection,
    state: &State,
    policy: Policy,
) -> Result<(Vec<Item>, Vec<&'static str>)> {
    let mut items = Vec::new();
    let mut unavailable = Vec::new();
    for p in providers {
        let mut wanted: BTreeMap<(String, String), toml::Value> = BTreeMap::new();
        for m in &sel.modules {
            for (domain, keys) in m.settings.get(p.name()).into_iter().flatten() {
                for (key, value) in keys {
                    wanted.insert((domain.clone(), key.clone()), value.clone());
                }
            }
        }
        if wanted.is_empty() {
            continue;
        }
        if !p.available() {
            unavailable.push(p.name());
            continue;
        }
        for ((domain, key), value) in wanted {
            let desired = p.normalise(&value);
            let machine = p.read(&domain, &key, policy)?;
            let mut item = Item {
                provider: p.name(),
                domain,
                key,
                value,
                desired,
                machine,
                outcome: Outcome::Ok,
            };
            let ledger = state.settings.get(&item.state_key()).map(String::as_str);
            item.outcome = outcome(ledger, item.machine.as_deref(), &item.desired);
            items.push(item);
        }
    }
    Ok((items, unavailable))
}

fn rows_for_unavailable(unavailable: &[&str], report: &mut Report) {
    for name in unavailable {
        report.push(
            Row::new("skipped", &format!("settings: {name}"))
                .module(&format!("{name} tool not on PATH"))
                .exit(1),
        );
    }
}

/// Rows for `rig status`: drift only, nothing written.
///
/// # Errors
/// When a provider cannot be queried.
pub fn status(
    providers: &[Box<dyn Provider>],
    sel: &Selection,
    state: &State,
    policy: Policy,
    report: &mut Report,
) -> Result<()> {
    let (items, unavailable) = plan(providers, sel, state, policy)?;
    rows_for_unavailable(&unavailable, report);
    for item in &items {
        match item.outcome {
            Outcome::Edited => report.push(edited_row(item)),
            Outcome::Set | Outcome::Updated => report.push(
                Row::new("setting", &item.label())
                    .module(&format!(
                        "pending: {}, repo: {}",
                        item.machine.as_deref().unwrap_or("unset"),
                        item.desired
                    ))
                    .exit(1),
            ),
            Outcome::Ok | Outcome::Adopted => {}
        }
    }
    Ok(())
}

fn edited_row(item: &Item) -> Row {
    Row::new("setting", &item.label())
        .module(&format!(
            "edited: {}, repo: {}",
            item.machine.as_deref().unwrap_or("unset"),
            item.desired
        ))
        .exit(1)
}

/// Writes the settings that differ and records every one rig now owns.
///
/// # Errors
/// When a provider cannot be queried.
///
/// # Panics
/// Never; every item names a provider from `providers`.
pub fn sync(
    providers: &[Box<dyn Provider>],
    sel: &Selection,
    state: &mut State,
    opts: Options,
    report: &mut Report,
) -> Result<()> {
    let (items, unavailable) = plan(providers, sel, state, opts.policy)?;
    rows_for_unavailable(&unavailable, report);
    let mut ok: BTreeMap<&str, usize> = BTreeMap::new();
    let mut declared: BTreeMap<&str, usize> = BTreeMap::new();
    let provider = |name: &str| providers.iter().find(|p| p.name() == name).unwrap();
    for item in &items {
        *declared.entry(item.provider).or_default() += 1;
        let (label, write) = match item.outcome {
            Outcome::Ok => {
                *ok.entry(item.provider).or_default() += 1;
                continue;
            }
            Outcome::Adopted => ("adopted", false),
            Outcome::Set => ("set", true),
            Outcome::Updated => ("updated", true),
            Outcome::Edited if opts.force => ("forced", true),
            Outcome::Edited => {
                report.push(edited_row(item));
                continue;
            }
        };
        if opts.dry_run {
            if write {
                report.push(Row::new("setting", &item.label()).module(&format!("would {label}")));
            }
            continue;
        }
        if write {
            if let Err(e) =
                provider(item.provider).write(&item.domain, &item.key, &item.value, opts.policy)
            {
                report.push(
                    Row::new("error", &format!("setting {}", item.label()))
                        .note(&format!("{e:#}"))
                        .exit(2),
                );
                continue;
            }
        }
        state
            .settings
            .insert(item.state_key(), item.desired.clone());
        let mut row = Row::new("setting", &item.label()).module(label);
        row.quiet = !write;
        report.push(row);
    }
    if !opts.dry_run {
        let live: std::collections::BTreeSet<String> = items.iter().map(Item::state_key).collect();
        let skipped = |k: &str| unavailable.iter().any(|p| k.starts_with(&format!("{p}:")));
        state.settings.retain(|k, _| live.contains(k) || skipped(k));
    }
    for (name, n) in ok {
        if n == declared[name] {
            report.push(Row::new("setting", &format!("{name}: {n} ok")).quietly());
        }
    }
    Ok(())
}

/// Every provider rig knows, whether or not it applies here.
#[must_use]
pub fn providers() -> Vec<Box<dyn Provider>> {
    vec![Box::new(macos::Defaults)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        assert_eq!(outcome(None, Some("15"), "15"), Outcome::Adopted);
        assert_eq!(outcome(None, Some("12"), "15"), Outcome::Set);
        assert_eq!(outcome(None, None, "15"), Outcome::Set);
        assert_eq!(outcome(Some("15"), Some("15"), "15"), Outcome::Ok);
        assert_eq!(outcome(Some("15"), Some("15"), "20"), Outcome::Updated);
        assert_eq!(outcome(Some("15"), Some("12"), "15"), Outcome::Edited);
        assert_eq!(outcome(Some("15"), None, "15"), Outcome::Edited);
        assert_eq!(outcome(Some("1.5"), Some("1.50"), "1.5"), Outcome::Ok);
    }
}
