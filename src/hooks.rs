//! Commands a module runs after its files were written.

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
    pub output: String,
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
            let run = HookRun {
                module: m.name.clone(),
                command: hook.after.clone(),
                status: match (wanted, dry_run) {
                    (false, _) => HookStatus::Skipped,
                    (true, true) => HookStatus::WouldRun,
                    (true, false) => HookStatus::Ran,
                },
                output: String::new(),
            };
            if run.status == HookStatus::Ran {
                out.push(execute(run, home, &sel.host, wrote));
            } else {
                out.push(run);
            }
        }
    }
    out
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

fn execute(mut run: HookRun, home: &Path, host: &str, wrote: &[PathBuf]) -> HookRun {
    let changed: Vec<String> = wrote.iter().map(|p| p.display().to_string()).collect();
    let result = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(&run.command)
        .current_dir(home)
        .env("RIG_MODULE", &run.module)
        .env("RIG_HOST", host)
        .env("RIG_CHANGED", changed.join("\n"))
        .output();
    match result {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            run.output = tail(&text);
            if !out.status.success() {
                run.status = HookStatus::Failed(out.status.code().unwrap_or(1));
            }
        }
        Err(e) => {
            run.status = HookStatus::Failed(1);
            run.output = e.to_string();
        }
    }
    run
}

/// One row per hook that fired, plus `(none)` for a module whose hooks all sat out.
///
/// `show_none` suppresses the `(none)` rows on a run that changed nothing at all;
/// `verbose` attaches each hook's captured output.
#[must_use]
pub fn rows(runs: &[HookRun], show_none: bool, verbose: bool) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::new();
    let mut fired: BTreeMap<&str, bool> = BTreeMap::new();
    for r in runs {
        let entry = fired.entry(&r.module).or_insert(false);
        match r.status {
            HookStatus::Skipped => {}
            HookStatus::Ran => {
                *entry = true;
                let mut row = Row::new("hook", &format!("{}: {}", r.module, r.command));
                if verbose && !r.output.is_empty() {
                    row = row.note(&r.output);
                }
                out.push(row);
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
                    .note(&r.output)
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
