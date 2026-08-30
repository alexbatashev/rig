//! The one place rig prints a summary row.

use crate::reconcile::{Action, Outcome, Plan};
use std::io::Write;

#[derive(Clone, Debug)]
pub struct Row {
    pub outcome: String,
    pub target: String,
    pub module: Option<String>,
    pub note: Option<String>,
    /// This row's contribution to the exit code.
    pub exit: i32,
    pub quiet: bool,
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
        }
    }
}

#[derive(Debug, Default)]
pub struct Report {
    pub rows: Vec<Row>,
}

impl Report {
    pub fn push(&mut self, row: Row) {
        self.rows.push(row);
    }

    #[must_use]
    pub fn exit(&self) -> i32 {
        self.rows.iter().map(|r| r.exit).max().unwrap_or(0)
    }

    /// # Errors
    /// When the writer fails.
    pub fn print(&self, out: &mut dyn Write, verbose: bool) -> std::io::Result<()> {
        let shown: Vec<&Row> = self.rows.iter().filter(|r| verbose || !r.quiet).collect();
        if shown.is_empty() {
            writeln!(out, "nothing to do")?;
            return Ok(());
        }
        let tw = shown.iter().map(|r| r.target.len()).max().unwrap_or(0) + 2;
        let mw = shown
            .iter()
            .filter_map(|r| r.module.as_ref().map(|m| m.len() + 2))
            .max()
            .unwrap_or(0)
            + 2;
        for r in shown {
            let module = r
                .module
                .as_ref()
                .map_or_else(String::new, |m| format!("({m})"));
            let line = format!(
                "  {:<11}{:<tw$}{:<mw$}{}",
                r.outcome,
                r.target,
                module,
                r.note.as_deref().unwrap_or("")
            );
            writeln!(out, "{}", line.trim_end())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_to_do_when_all_rows_are_quiet() {
        let mut r = Report::default();
        let mut row = Row::new("unchanged", "~/f");
        row.quiet = true;
        r.push(row);
        let mut out = Vec::new();
        r.print(&mut out, false).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "nothing to do\n");
    }

    #[test]
    fn columns_line_up_and_lines_have_no_trailing_space() {
        let mut r = Report::default();
        r.push(Row::new("created", "~/a").module("m"));
        r.push(Row::new("orphaned", "~/long/target/name").note("run rig up --prune"));
        let mut out = Vec::new();
        r.print(&mut out, false).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.lines().all(|l| l == l.trim_end()), "{text}");
        assert_eq!(
            text.lines().next().unwrap(),
            "  created    ~/a                 (m)"
        );
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
