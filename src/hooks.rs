//! Commands a module runs after its files were written.

use crate::exec::{try_run, Cmd, Policy};
use crate::repo::{HookWhen, Selection};
use crate::report::Row;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookStatus {
    Ran,
    Failed(i32),
    Skipped,
    WouldRun,
}

#[derive(Clone, Debug)]
pub struct HookRun {
    pub module: String,
    pub command: String,
    pub status: HookStatus,
}

/// Runs each active module's hooks in host order, after files and packages.
#[must_use]
pub fn run_hooks(
    sel: &Selection,
    changed: &BTreeMap<String, Vec<PathBuf>>,
    home: &Path,
    dry_run: bool,
) -> Vec<HookRun> {
    let mut out = Vec::new();
    for m in &sel.modules {
        let wrote = changed.get(&m.name).map_or(&[][..], Vec::as_slice);
        for hook in &m.hooks {
            let wanted = hook.when == HookWhen::Always || !wrote.is_empty();
            let mut run = HookRun {
                module: m.name.clone(),
                command: hook.after.clone(),
                status: match (wanted, dry_run) {
                    (false, _) => HookStatus::Skipped,
                    (true, true) => HookStatus::WouldRun,
                    (true, false) => HookStatus::Ran,
                },
            };
            if run.status == HookStatus::Ran {
                run.status = execute(&run, home, &sel.host, wrote);
            }
            out.push(run);
        }
    }
    out
}

fn execute(run: &HookRun, home: &Path, host: &str, wrote: &[PathBuf]) -> HookStatus {
    let changed: Vec<String> = wrote.iter().map(|p| p.display().to_string()).collect();
    let cmd = Cmd::new("/bin/sh", &["-c", &run.command])
        .env("RIG_MODULE", &run.module)
        .env("RIG_HOST", host)
        .env("RIG_CHANGED", &changed.join("\n"))
        .in_dir(home.to_path_buf());
    match try_run(&cmd, Policy::default()) {
        Ok(done) if done.ok() => HookStatus::Ran,
        Ok(done) => HookStatus::Failed(done.code),
        Err(e) => {
            eprintln!("    {e:#}");
            HookStatus::Failed(1)
        }
    }
}

/// One row per hook that fired, plus `(none)` for a module whose hooks all sat out.
///
/// `show_none` suppresses the `(none)` rows on a run that changed nothing at all.
#[must_use]
pub fn rows(runs: &[HookRun], show_none: bool) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    let mut fired: BTreeMap<&str, bool> = BTreeMap::new();
    for r in runs {
        let entry = fired.entry(&r.module).or_insert(false);
        match r.status {
            HookStatus::Skipped => {}
            HookStatus::Ran => {
                *entry = true;
                out.push(Row::new("hook", &format!("{}: {}", r.module, r.command)));
            }
            HookStatus::WouldRun => {
                *entry = true;
                out.push(Row::new(
                    "hook",
                    &format!("{}: would run {}", r.module, r.command),
                ));
            }
            HookStatus::Failed(code) => {
                *entry = true;
                out.push(
                    Row::new(
                        "hook",
                        &format!("{}: FAILED (exit {code}): {}", r.module, r.command),
                    )
                    .exit(2),
                );
            }
        }
    }
    for (module, ran) in fired {
        if !ran && show_none {
            out.push(Row::new("hook", &format!("{module}: (none)")));
        }
    }
    out
}
