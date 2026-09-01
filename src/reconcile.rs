//! The nine cell matrix, as one pure function.

use crate::compose::Desired;
use crate::repo::Target;
use crate::state::{BlobSource, Hash, State};
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Reads what is currently on disk for a target.
pub trait Disk {
    /// `None` means the file is absent. A dangling symlink reads as absent.
    ///
    /// # Errors
    /// When the file exists but cannot be read.
    fn read(&self, target: &Target) -> Result<Option<Vec<u8>>>;
    /// Where the path points when it is a symlink, which means someone else owns it.
    fn link_target(&self, target: &Target) -> Option<PathBuf>;
}

#[derive(Clone, Debug, Default)]
pub enum Force {
    #[default]
    None,
    All,
    Only(BTreeSet<Target>),
}

impl Force {
    fn covers(&self, t: &Target) -> bool {
        match self {
            Force::None => false,
            Force::All => true,
            Force::Only(s) => s.contains(t),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub force: Force,
    pub adopt: bool,
    pub prune: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Write `content` to disk and record `record` in state.
    Write {
        content: Vec<u8>,
        mode: u32,
        record: Vec<u8>,
    },
    /// State only; the disk already holds this content.
    Record {
        content: Vec<u8>,
        mode: u32,
    },
    /// Drop the state entry; the file is already gone.
    Forget,
    Delete,
    /// Write conflict markers under `conflicts/`; the disk is untouched.
    Conflict {
        marked: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    UnmanagedDiffers,
    /// The path is a symlink rig never wrote, whatever it points at.
    ForeignSymlink,
    Merge,
    MissingBlob,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Adopted,
    Unchanged,
    Updated,
    Edited,
    Merged,
    Deleted,
    Orphaned,
    Conflict(ConflictKind),
}

impl Outcome {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Created => "created",
            Outcome::Adopted => "adopted",
            Outcome::Unchanged => "unchanged",
            Outcome::Updated => "updated",
            Outcome::Edited => "edited",
            Outcome::Merged => "merged",
            Outcome::Deleted => "deleted",
            Outcome::Orphaned => "orphaned",
            Outcome::Conflict(_) => "conflict",
        }
    }

    /// Rows that are only interesting under `-v`.
    #[must_use]
    pub fn is_quiet(self) -> bool {
        matches!(self, Outcome::Unchanged)
    }
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub target: Target,
    pub module: Option<String>,
    pub outcome: Outcome,
    pub action: Action,
    pub note: Option<String>,
    /// Content to keep as a blob before it is overwritten (cell 3 with `--adopt`).
    pub preserve: Option<Vec<u8>>,
}

impl Plan {
    fn new(target: Target, module: Option<String>, outcome: Outcome, action: Action) -> Plan {
        Plan {
            target,
            module,
            outcome,
            action,
            note: None,
            preserve: None,
        }
    }
}

fn write(d: &Desired) -> Action {
    Action::Write {
        content: d.content.clone(),
        mode: d.mode,
        record: d.content.clone(),
    }
}

fn plan_for(
    d: &Desired,
    state: &State,
    disk: &dyn Disk,
    blobs: &dyn BlobSource,
    opts: &Options,
) -> Result<Plan> {
    let module = Some(d.module.clone());
    let target = d.target.clone();
    let forced = opts.force.covers(&target);
    let entry = state.entries.get(&target);
    let on_disk = disk.read(&target)?;
    let dh = Hash::of(&d.content);

    let mk = |o, a| Plan::new(target.clone(), module.clone(), o, a);

    let (Some(entry), Some(k)) = (entry, on_disk.as_ref()) else {
        return Ok(match (entry, on_disk) {
            // cells 1 and 4: absent is absent, whether or not rig wrote it once.
            (None | Some(_), None) => mk(Outcome::Created, write(d)),
            // cell 2 and 3
            (None, Some(k)) => {
                if let Some(link) = disk.link_target(&target) {
                    if opts.adopt || forced {
                        let mut p = mk(Outcome::Adopted, write(d));
                        p.note = Some(format!("was a symlink to {}", link.display()));
                        p
                    } else {
                        let mut p = mk(
                            Outcome::Conflict(ConflictKind::ForeignSymlink),
                            Action::Nothing,
                        );
                        p.note = Some(format!(
                            "symlink to {}; rig up --adopt {target} replaces it",
                            link.display()
                        ));
                        p
                    }
                } else if Hash::of(&k) == dh {
                    mk(
                        Outcome::Adopted,
                        Action::Record {
                            content: d.content.clone(),
                            mode: d.mode,
                        },
                    )
                } else if opts.adopt {
                    let mut p = mk(Outcome::Adopted, write(d));
                    p.note = Some(format!(
                        "previous content kept as blob {}",
                        Hash::of(&k).short()
                    ));
                    p.preserve = Some(k);
                    p
                } else if forced {
                    mk(Outcome::Adopted, write(d))
                } else {
                    mk(
                        Outcome::Conflict(ConflictKind::UnmanagedDiffers),
                        Action::Nothing,
                    )
                }
            }
            (Some(_), Some(_)) => unreachable!(),
        });
    };

    if Hash::of(k) == entry.hash {
        // cells 5 and 6
        return Ok(if dh == entry.hash {
            mk(Outcome::Unchanged, Action::Nothing)
        } else {
            mk(Outcome::Updated, write(d))
        });
    }
    if dh == entry.hash {
        // cell 7
        return Ok(if forced {
            mk(Outcome::Updated, write(d))
        } else {
            let mut p = mk(Outcome::Edited, Action::Nothing);
            p.note = Some("hint: rig absorb".into());
            p
        });
    }
    // cells 8 and 9
    if forced {
        return Ok(mk(Outcome::Updated, write(d)));
    }
    let Some(base) = blobs.blob(&entry.hash)? else {
        let mut p = mk(
            Outcome::Conflict(ConflictKind::MissingBlob),
            Action::Nothing,
        );
        p.note = Some(format!(
            "state blob missing, run rig up --force {target} or --adopt"
        ));
        return Ok(p);
    };
    Ok(match diffy::merge_bytes(&base, k, &d.content) {
        Ok(merged) => mk(
            Outcome::Merged,
            Action::Write {
                content: merged,
                mode: d.mode,
                record: d.content.clone(),
            },
        ),
        Err(marked) => mk(
            Outcome::Conflict(ConflictKind::Merge),
            Action::Conflict { marked },
        ),
    })
}

/// Turns desired content, recorded state and the disk into one plan per target.
///
/// # Errors
/// When the disk or the blob store cannot be read.
pub fn reconcile(
    desired: &[Desired],
    state: &State,
    disk: &dyn Disk,
    blobs: &dyn BlobSource,
    opts: &Options,
) -> Result<Vec<Plan>> {
    let mut plans = Vec::new();
    for d in desired {
        plans.push(plan_for(d, state, disk, blobs, opts)?);
    }
    let managed: BTreeSet<&Target> = desired.iter().map(|d| &d.target).collect();
    for (target, entry) in &state.entries {
        if managed.contains(target) {
            continue;
        }
        let module = Some(entry.module.clone());
        plans.push(match disk.read(target)? {
            None => Plan::new(target.clone(), module, Outcome::Deleted, Action::Forget),
            Some(_) if disk.link_target(target).is_some() => {
                let mut p = Plan::new(target.clone(), module, Outcome::Orphaned, Action::Forget);
                p.note = Some("foreign symlink, kept".into());
                p
            }
            Some(k) if Hash::of(&k) == entry.hash => {
                let mut p = Plan::new(
                    target.clone(),
                    module,
                    Outcome::Orphaned,
                    if opts.prune {
                        Action::Delete
                    } else {
                        Action::Nothing
                    },
                );
                if !opts.prune {
                    p.note = Some("run rig up --prune".into());
                }
                p
            }
            Some(_) => {
                let mut p = Plan::new(target.clone(), module, Outcome::Orphaned, Action::Nothing);
                p.note = Some("edited, kept".into());
                p
            }
        });
    }
    plans.sort_by(|a, b| a.target.cmp(&b.target));
    Ok(plans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Format;
    use crate::state::Entry;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct MemDisk(BTreeMap<Target, Vec<u8>>, BTreeMap<Target, PathBuf>);
    impl Disk for MemDisk {
        fn read(&self, target: &Target) -> Result<Option<Vec<u8>>> {
            Ok(self.0.get(target).cloned())
        }
        fn link_target(&self, target: &Target) -> Option<PathBuf> {
            self.1.get(target).cloned()
        }
    }

    #[derive(Default)]
    struct MemBlobs(BTreeMap<Hash, Vec<u8>>);
    impl BlobSource for MemBlobs {
        fn blob(&self, hash: &Hash) -> Result<Option<Vec<u8>>> {
            Ok(self.0.get(hash).cloned())
        }
    }

    fn target() -> Target {
        "~/f".parse().unwrap()
    }

    fn desired(content: &str) -> Desired {
        Desired {
            target: target(),
            content: content.as_bytes().to_vec(),
            mode: 0o644,
            module: "m".into(),
            format: Format::Text,
            layers: Vec::new(),
        }
    }

    struct Case {
        state: State,
        disk: MemDisk,
        blobs: MemBlobs,
        opts: Options,
    }

    impl Case {
        fn new() -> Case {
            Case {
                state: State::default(),
                disk: MemDisk::default(),
                blobs: MemBlobs::default(),
                opts: Options::default(),
            }
        }

        fn last_written(mut self, content: &str) -> Case {
            let h = Hash::of(content.as_bytes());
            self.blobs.0.insert(h, content.as_bytes().to_vec());
            self.state.entries.insert(
                target(),
                Entry {
                    hash: h,
                    module: "m".into(),
                    mode: 0o644,
                },
            );
            self
        }

        fn on_disk(mut self, content: &str) -> Case {
            self.disk.0.insert(target(), content.as_bytes().to_vec());
            self
        }

        fn linked_to(mut self, content: &str, link: &str) -> Case {
            self.disk.1.insert(target(), PathBuf::from(link));
            self.on_disk(content)
        }

        fn run(&self, d: &str) -> Plan {
            let mut p = reconcile(
                &[desired(d)],
                &self.state,
                &self.disk,
                &self.blobs,
                &self.opts,
            )
            .unwrap();
            p.remove(0)
        }
    }

    fn written(p: &Plan) -> (&[u8], &[u8]) {
        match &p.action {
            Action::Write {
                content, record, ..
            } => (content, record),
            other => panic!("expected a write, got {other:?}"),
        }
    }

    #[test]
    fn cell1_created() {
        let p = Case::new().run("D\n");
        assert_eq!(p.outcome, Outcome::Created);
        assert_eq!(written(&p).0, b"D\n");
    }

    #[test]
    fn cell2_adopted() {
        let p = Case::new().on_disk("D\n").run("D\n");
        assert_eq!(p.outcome, Outcome::Adopted);
        assert_eq!(
            p.action,
            Action::Record {
                content: b"D\n".to_vec(),
                mode: 0o644,
            }
        );
    }

    #[test]
    fn cell3_conflict() {
        let p = Case::new().on_disk("K\n").run("D\n");
        assert_eq!(p.outcome, Outcome::Conflict(ConflictKind::UnmanagedDiffers));
        assert_eq!(p.action, Action::Nothing);
    }

    #[test]
    fn cell3_adopt_flag() {
        let mut c = Case::new().on_disk("K\n");
        c.opts.adopt = true;
        let p = c.run("D\n");
        assert_eq!(p.outcome, Outcome::Adopted);
        assert_eq!(written(&p), (&b"D\n"[..], &b"D\n"[..]));
        assert_eq!(p.preserve.as_deref(), Some(&b"K\n"[..]));
    }

    #[test]
    fn cell3_force_flag() {
        let mut c = Case::new().on_disk("K\n");
        c.opts.force = Force::All;
        let p = c.run("D\n");
        assert_eq!(written(&p).0, b"D\n");
        assert!(p.preserve.is_none());
    }

    #[test]
    fn cell4_recreates() {
        let p = Case::new().last_written("D\n").run("D\n");
        assert_eq!(p.outcome, Outcome::Created);
        assert_eq!(written(&p).0, b"D\n");
    }

    #[test]
    fn symlink_is_foreign_until_adopted() {
        let c = Case::new().linked_to("D\n", "/nix/store/x/D");
        let p = c.run("D\n");
        assert_eq!(p.outcome, Outcome::Conflict(ConflictKind::ForeignSymlink));
        assert_eq!(p.action, Action::Nothing);
        assert!(p.note.unwrap().contains("/nix/store/x/D"));

        let mut c = Case::new().linked_to("K\n", "/nix/store/x/D");
        c.opts.adopt = true;
        let p = c.run("D\n");
        assert_eq!(p.outcome, Outcome::Adopted);
        assert_eq!(written(&p).0, b"D\n");
        assert!(p.preserve.is_none());
    }

    #[test]
    fn cell5_unchanged() {
        let p = Case::new().last_written("D\n").on_disk("D\n").run("D\n");
        assert_eq!(p.outcome, Outcome::Unchanged);
        assert_eq!(p.action, Action::Nothing);
    }

    #[test]
    fn cell6_updated() {
        let p = Case::new().last_written("L\n").on_disk("L\n").run("D\n");
        assert_eq!(p.outcome, Outcome::Updated);
        assert_eq!(written(&p).0, b"D\n");
    }

    #[test]
    fn cell7_edited() {
        let p = Case::new().last_written("L\n").on_disk("K\n").run("L\n");
        assert_eq!(p.outcome, Outcome::Edited);
        assert_eq!(p.action, Action::Nothing);
    }

    #[test]
    fn cell7_force() {
        let mut c = Case::new().last_written("L\n").on_disk("K\n");
        c.opts.force = Force::All;
        let p = c.run("L\n");
        assert_eq!(written(&p).0, b"L\n");
    }

    #[test]
    fn cell8_merged_records_desired() {
        let base = "one\ntwo\nthree\nfour\nfive\n";
        let disk = "one\ntwo\nthree\nfour\nEDITED\n";
        let repo = "NEW\ntwo\nthree\nfour\nfive\n";
        let p = Case::new().last_written(base).on_disk(disk).run(repo);
        assert_eq!(p.outcome, Outcome::Merged);
        let (content, record) = written(&p);
        assert_eq!(content, b"NEW\ntwo\nthree\nfour\nEDITED\n");
        assert_eq!(record, repo.as_bytes());
    }

    #[test]
    fn cell9_conflict_markers_label_disk_as_ours() {
        let p = Case::new()
            .last_written("a\n")
            .on_disk("mine\n")
            .run("theirs\n");
        assert_eq!(p.outcome, Outcome::Conflict(ConflictKind::Merge));
        let Action::Conflict { marked } = &p.action else {
            panic!("expected a conflict")
        };
        let text = String::from_utf8_lossy(marked);
        let ours = text.find("mine").unwrap();
        let sep = text.find("=======").unwrap();
        assert!(ours < sep, "{text}");
        assert!(text.find("theirs").unwrap() > sep, "{text}");
    }

    #[test]
    fn cell9_force() {
        let mut c = Case::new().last_written("a\n").on_disk("mine\n");
        c.opts.force = Force::All;
        let p = c.run("theirs\n");
        assert_eq!(written(&p), (&b"theirs\n"[..], &b"theirs\n"[..]));
    }

    #[test]
    fn missing_blob_reports_conflict() {
        let mut c = Case::new().last_written("a\n").on_disk("mine\n");
        c.blobs.0.clear();
        let p = c.run("theirs\n");
        assert_eq!(p.outcome, Outcome::Conflict(ConflictKind::MissingBlob));
        assert_eq!(p.action, Action::Nothing);
    }

    #[test]
    fn force_only_path_leaves_other_targets_default() {
        let other: Target = "~/other".parse().unwrap();
        let mut c = Case::new().on_disk("K\n");
        c.disk.0.insert(other.clone(), b"K2\n".to_vec());
        c.opts.force = Force::Only([target()].into());
        let mut d2 = desired("D2\n");
        d2.target = other;
        let plans = reconcile(&[desired("D\n"), d2], &c.state, &c.disk, &c.blobs, &c.opts).unwrap();
        assert_eq!(plans[0].outcome, Outcome::Adopted);
        assert_eq!(
            plans[1].outcome,
            Outcome::Conflict(ConflictKind::UnmanagedDiffers)
        );
    }

    fn orphan(disk: Option<&str>, prune: bool) -> Plan {
        let mut c = Case::new().last_written("L\n");
        if let Some(k) = disk {
            c = c.on_disk(k);
        }
        c.opts.prune = prune;
        let mut plans = reconcile(&[], &c.state, &c.disk, &c.blobs, &c.opts).unwrap();
        plans.remove(0)
    }

    #[test]
    fn orphan_kept() {
        let p = orphan(Some("L\n"), false);
        assert_eq!(p.outcome, Outcome::Orphaned);
        assert_eq!(p.action, Action::Nothing);
    }

    #[test]
    fn orphan_pruned() {
        assert_eq!(orphan(Some("L\n"), true).action, Action::Delete);
    }

    #[test]
    fn orphan_edited_not_pruned() {
        let p = orphan(Some("edited\n"), true);
        assert_eq!(p.outcome, Outcome::Orphaned);
        assert_eq!(p.action, Action::Nothing);
    }

    #[test]
    fn orphan_symlink_is_forgotten_not_deleted() {
        let mut c = Case::new()
            .last_written("L\n")
            .linked_to("L\n", "/elsewhere");
        c.opts.prune = true;
        let mut plans = reconcile(&[], &c.state, &c.disk, &c.blobs, &c.opts).unwrap();
        let p = plans.remove(0);
        assert_eq!(p.outcome, Outcome::Orphaned);
        assert_eq!(p.action, Action::Forget);
    }

    #[test]
    fn orphan_already_gone_dropped_from_state() {
        let p = orphan(None, false);
        assert_eq!(p.outcome, Outcome::Deleted);
        assert_eq!(p.action, Action::Forget);
    }
}
