//! The only module that writes managed files.

use crate::reconcile::{Action, Disk, Plan};
use crate::repo::{Roots, Target};
use crate::report::{Report, Row};
use crate::state::{write_atomic, Entry, State, Store};
use anyhow::{Context, Result};

pub struct RealDisk {
    pub roots: Roots,
}

impl Disk for RealDisk {
    fn read(&self, target: &Target) -> Result<Option<Vec<u8>>> {
        let path = target.resolve(&self.roots);
        match std::fs::read(&path) {
            Ok(c) => Ok(Some(c)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn link_target(&self, target: &Target) -> Option<std::path::PathBuf> {
        std::fs::read_link(target.resolve(&self.roots)).ok()
    }
}

fn record(state: &mut State, store: &Store, plan: &Plan, content: &[u8], mode: u32) -> Result<()> {
    let hash = store.put_blob(content)?;
    state.entries.insert(
        plan.target.clone(),
        Entry {
            hash,
            module: plan.module.clone().unwrap_or_default(),
            mode,
        },
    );
    Ok(())
}

fn perform(plan: &Plan, roots: &Roots, store: &Store, state: &mut State) -> Result<()> {
    match &plan.action {
        Action::Nothing => {}
        Action::Write {
            content,
            mode,
            record: recorded,
        } => {
            if let Some(previous) = &plan.preserve {
                store.pin(store.put_blob(previous)?);
            }
            write_atomic(&plan.target.resolve(roots), content, *mode)?;
            record(state, store, plan, recorded, *mode)?;
        }
        Action::Record { content, mode } => {
            record(state, store, plan, content, *mode)?;
        }
        Action::Forget => {
            state.entries.remove(&plan.target);
        }
        Action::Delete => {
            let path = plan.target.resolve(roots);
            std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            state.entries.remove(&plan.target);
        }
        Action::Conflict { marked } => {
            store.write_conflict(&plan.target, marked)?;
        }
    }
    if !matches!(plan.action, Action::Conflict { .. }) {
        store.clear_conflict(&plan.target)?;
    }
    Ok(())
}

/// Carries out the plans in target order, pushing one row per plan as it goes.
///
/// # Errors
/// When the manifest cannot be saved.
pub fn apply(
    plans: &[Plan],
    roots: &Roots,
    store: &Store,
    state: &mut State,
    dry_run: bool,
    report: &mut Report,
) -> Result<()> {
    for plan in plans {
        if dry_run {
            report.push(dry_row(plan));
            continue;
        }
        match perform(plan, roots, store, state) {
            Ok(()) => report.push(Row::from_plan(plan)),
            Err(e) => {
                report.push(
                    Row::new("error", &plan.target.to_string())
                        .note(&format!("{e:#}"))
                        .exit(2),
                );
                store.save(state)?;
            }
        }
    }
    if !dry_run {
        store.save(state)?;
    }
    Ok(())
}

/// The row a plan would produce, marked for `--dry-run`.
#[must_use]
pub fn dry_row(plan: &Plan) -> Row {
    let mut row = Row::from_plan(plan);
    if matches!(plan.action, Action::Write { .. }) {
        row.note = Some(match &row.note {
            Some(n) => format!("would write, {n}"),
            None => "would write".to_string(),
        });
    }
    row
}
