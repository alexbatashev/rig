//! The one place rig prints a summary row.

use crate::reconcile::{Action, Outcome, Plan};

#[derive(Clone, Debug)]
pub struct Row {
    pub outcome: String,
    pub target: String,
    pub module: Option<String>,
    pub note: Option<String>,
    /// This row's contribution to the exit code.
    pub exit: i32,
    pub quiet: bool,
    /// Whether this row put bytes on disk, which is what a `changed` hook waits for.
    pub wrote: bool,
}

impl Row {
    #[must_use]
    pub fn new(outcome: &str, target: &str) -> Row {
        Row {
            outcome: outcome.to_string(),
            target: target.to_string(),
            module: None,
            note: None,
            exit: 0,
            quiet: false,
            wrote: false,
        }
    }

    #[must_use]
    pub fn module(mut self, module: &str) -> Row {
        self.module = Some(module.to_string());
        self
    }

    #[must_use]
    pub fn note(mut self, note: &str) -> Row {
        self.note = Some(note.to_string());
        self
    }

    /// The same row, printed only under `-v` because someone else already showed it.
    #[must_use]
    pub fn quietly(mut self) -> Row {
        self.quiet = true;
        self
    }

    #[must_use]
    pub fn exit(mut self, exit: i32) -> Row {
        self.exit = exit;
        self
    }

    /// An orphan whose file is already gone leaves nothing to report.
    #[must_use]
    pub fn from_plan(plan: &Plan) -> Row {
        let forgotten = matches!(plan.action, Action::Forget);
        let exit = match plan.outcome {
            _ if forgotten => 0,
            Outcome::Edited | Outcome::Conflict(_) | Outcome::Deleted | Outcome::Orphaned => 1,
            _ => 0,
        };
        Row {
            outcome: plan.outcome.label().to_string(),
            target: plan.target.to_string(),
            module: plan.module.clone(),
            note: plan.note.clone(),
            exit,
            quiet: plan.outcome.is_quiet() || forgotten,
            wrote: matches!(plan.action, Action::Write { .. }),
        }
    }

    /// The row as one printed line, in fixed columns so rows can stream.
    #[must_use]
    pub fn line(&self) -> String {
        let module = self
            .module
            .as_ref()
            .map_or_else(String::new, |m| format!("({m})"));
        let line = format!(
            "  {:<11}{:<52}{:<14}{}",
            self.outcome,
            self.target,
            module,
            self.note.as_deref().unwrap_or("")
        );
        line.trim_end().to_string()
    }
}

/// Rows in the order they were decided. A live report prints each one as it arrives.
#[derive(Debug, Default)]
pub struct Report {
    pub rows: Vec<Row>,
    live: Option<bool>,
    shown: usize,
}

impl Report {
    /// A report that prints rows to stdout as they are pushed; `verbose` includes quiet rows.
    #[must_use]
    pub fn live(verbose: bool) -> Report {
        Report {
            rows: Vec::new(),
            live: Some(verbose),
            shown: 0,
        }
    }

    pub fn push(&mut self, row: Row) {
        if let Some(verbose) = self.live {
            if verbose || !row.quiet {
                println!("{}", row.line());
                self.shown += 1;
            }
        }
        self.rows.push(row);
    }

    /// Says so when a live run printed nothing.
    pub fn finish(&self) {
        if self.live.is_some() && self.shown == 0 {
            println!("nothing to do");
        }
    }

    #[must_use]
    pub fn exit(&self) -> i32 {
        self.rows.iter().map(|r| r.exit).max().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_has_fixed_columns_and_no_trailing_space() {
        let row = Row::new("created", "~/a").module("m");
        assert_eq!(row.line(), format!("  {:<11}{:<52}(m)", "created", "~/a"));
        let row = Row::new("orphaned", "~/long/target/name").note("run rig up --prune");
        assert_eq!(row.line(), row.line().trim_end());
        assert!(row.line().ends_with("run rig up --prune"));
    }

    #[test]
    fn exit_is_the_worst_row() {
        let mut r = Report::default();
        r.push(Row::new("created", "~/a"));
        r.push(Row::new("edited", "~/b").exit(1));
        assert_eq!(r.exit(), 1);
        r.push(Row::new("error", "~/c").exit(2));
        assert_eq!(r.exit(), 2);
    }
}
