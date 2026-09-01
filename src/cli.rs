//! Command line entry points.

use crate::absorb::{absorb, equivalent, summary, AbsorbEdit, Lower, TopLayer};
use crate::apply::{apply, dry_row, RealDisk};
use crate::compose::{compose, desired, layers, Desired, Layer};
use crate::exec::{is_root, Confirm, Policy};
use crate::hooks::{self, run_hooks};
use crate::packages;
use crate::reconcile::{reconcile, Action, Disk, Force, Options, Outcome, Plan};
use crate::repo::{load, select, Os, Repo, Roots, Selection, Sync, Target, KNOWN_OS_TAGS};
use crate::report::{Report, Row};
use crate::settings;
use crate::state::{write_atomic, BlobSource, Entry, Hash, State, Store};
use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "rig",
    version,
    about = "Compose, apply and absorb config files"
)]
pub struct Cli {
    /// Remap `/etc` and `/var/lib/rig` under this directory. Tests only.
    #[arg(long, hide = true, global = true)]
    etc_root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct UpArgs {
    /// Repo path. Remembered in `~/.config/rig/repo`.
    source: Option<PathBuf>,
    #[arg(long)]
    host: Option<String>,
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Answer rig's questions with yes.
    #[arg(short = 'y')]
    yes: bool,
    /// Take the repo's version of files rig does not manage yet.
    #[arg(long)]
    adopt: bool,
    /// Overwrite everything, or just the given path. Repeatable.
    #[arg(long, num_args = 0..=1, default_missing_value = "", action = clap::ArgAction::Append)]
    force: Vec<String>,
    /// Delete files that left the repo and were not edited.
    #[arg(long)]
    prune: bool,
    /// Never escalate for `/etc` targets.
    #[arg(long)]
    no_sudo: bool,
    #[arg(short = 'v', long)]
    verbose: bool,
    #[arg(long, hide = true)]
    etc_only: bool,
    #[arg(long, hide = true)]
    repo: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Put every managed file on disk.
    Up(UpArgs),
    /// One row per target that is not in the steady state.
    Status {
        #[arg(long)]
        host: Option<String>,
        /// Mode drift on disk is not detected; the hash covers content only.
        #[arg(short = 'v', long)]
        verbose: bool,
    },
    /// Unified diff of desired against disk.
    Diff {
        path: Option<String>,
        #[arg(long)]
        host: Option<String>,
    },
    /// Carry an edit made on disk back into the repo.
    Absorb {
        path: Option<String>,
        /// Every edited file whose module syncs automatically.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        host: Option<String>,
        #[arg(short = 'v', long)]
        verbose: bool,
    },
    /// Start managing a file that is already on disk.
    Adopt {
        path: String,
        #[arg(long)]
        module: String,
        /// Land it in an `@<host>` variant.
        #[arg(long)]
        host_variant: bool,
        /// Land it in an `@<os>` variant.
        #[arg(long = "os")]
        os_variant: bool,
        #[arg(long)]
        host: Option<String>,
    },
    /// Open each conflict in $EDITOR and take the result.
    Resolve {
        path: Option<String>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long, hide = true)]
        etc_only: bool,
        #[arg(long, hide = true)]
        repo: Option<PathBuf>,
    },
    /// Create a repo skeleton and register this host.
    Init {
        dir: Option<PathBuf>,
        #[arg(long)]
        host: Option<String>,
    },
    /// Report repo parse errors and suspicious variant tags.
    Doctor { repo: Option<PathBuf> },
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
    let roots = Roots::new(home()?, cli.etc_root.as_deref());
    match cli.command {
        Command::Up(args) => up(&args, &roots, cli.etc_root.as_deref()),
        Command::Status { host, verbose } => status(&roots, host.as_deref(), verbose),
        Command::Diff { path, host } => diff(&roots, path.as_deref(), host.as_deref()),
        Command::Absorb {
            path,
            all,
            host,
            verbose,
        } => absorb_cmd(&roots, path.as_deref(), all, host.as_deref(), verbose),
        Command::Adopt {
            path,
            module,
            host_variant,
            os_variant,
            host,
        } => adopt(
            &roots,
            &path,
            &module,
            host_variant,
            os_variant,
            host.as_deref(),
        ),
        Command::Resolve {
            path,
            host,
            etc_only,
            repo,
        } => resolve(
            &roots,
            path.as_deref(),
            host.as_deref(),
            etc_only,
            repo.as_deref(),
            cli.etc_root.as_deref(),
        ),
        Command::Init { dir, host } => init(dir.as_deref(), host.as_deref()),
        Command::Doctor { repo } => {
            let path = match repo {
                Some(p) => p,
                None => repo_path(None, false)?,
            };
            Ok(doctor(&path))
        }
        Command::Compose {
            host,
            os,
            out,
            repo,
        } => compose_cmd(&repo, &host, &Os::parse(&os), out.as_deref()),
    }
}

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

fn config_dir() -> Result<PathBuf> {
    Ok(match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) => PathBuf::from(d),
        None => home()?.join(".config"),
    }
    .join("rig"))
}

fn home_state_dir() -> Result<PathBuf> {
    Ok(match std::env::var_os("XDG_STATE_HOME") {
        Some(d) => PathBuf::from(d),
        None => home()?.join(".local/state"),
    }
    .join("rig"))
}

fn etc_state_dir(etc_root: Option<&Path>) -> PathBuf {
    etc_root.map_or_else(|| PathBuf::from("/var/lib/rig"), |r| r.join("var/lib/rig"))
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is a valid writable buffer of the length passed in.
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
    if !ok {
        return "localhost".to_string();
    }
    let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).into_owned();
    name.split('.').next().unwrap_or("localhost").to_string()
}

fn repo_path(source: Option<&Path>, remember: bool) -> Result<PathBuf> {
    if let Some(p) = source {
        let abs = p
            .canonicalize()
            .with_context(|| format!("no such repo: {}", p.display()))?;
        if remember {
            let dir = config_dir()?;
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("repo"), format!("{}\n", abs.display()))?;
        }
        return Ok(abs);
    }
    if let Ok(text) = std::fs::read_to_string(config_dir()?.join("repo")) {
        let p = PathBuf::from(text.trim());
        if p.is_dir() {
            return Ok(p);
        }
    }
    let default = home()?.join("dotfiles");
    if default.is_dir() {
        return Ok(default);
    }
    bail!("no repo: run rig up <path> or rig init")
}

fn host_name(requested: Option<&str>, remember: bool) -> Result<String> {
    if let Some(h) = requested {
        if remember {
            let dir = config_dir()?;
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("host"), format!("{h}\n"))?;
        }
        return Ok(h.to_string());
    }
    if let Ok(text) = std::fs::read_to_string(config_dir()?.join("host")) {
        if !text.trim().is_empty() {
            return Ok(text.trim().to_string());
        }
    }
    Ok(hostname())
}

/// Everything the pipeline needs before reconciliation.
struct Session {
    repo: Repo,
    host: String,
    os: Os,
}

impl Session {
    fn selection(&self) -> Result<Selection<'_>> {
        select(&self.repo, &self.host, &self.os)
    }

    fn desired(&self) -> Result<Vec<Desired>> {
        desired(&self.repo, &self.selection()?, &self.os)
    }

    fn layers(&self) -> Result<BTreeMap<Target, Vec<Layer>>> {
        layers(&self.repo, &self.selection()?, &self.os)
    }

    fn rel(&self, path: &Path) -> String {
        path.strip_prefix(&self.repo.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

fn open(source: Option<&Path>, host: Option<&str>, remember: bool) -> Result<Session> {
    let path = repo_path(source, remember)?;
    let repo = load(&path)?;
    let host = host_name(host, remember)?;
    if !repo.hosts.contains_key(&host) {
        let known: Vec<&str> = repo.hosts.keys().map(String::as_str).collect();
        bail!("unknown host '{host}'; repo has: {}", known.join(", "));
    }
    Ok(Session {
        repo,
        host,
        os: Os::detect(),
    })
}

fn canonical(path: &Path) -> PathBuf {
    if let Ok(p) = path.canonicalize() {
        return p;
    }
    // The file may not exist yet; canonicalise the deepest existing ancestor.
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => canonical(dir).join(name),
        _ => path.to_path_buf(),
    }
}

fn resolve_target(path: &str, roots: &Roots) -> Result<Target> {
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        roots.home.join(rest)
    } else {
        let p = PathBuf::from(path);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir()?.join(p)
        }
    };
    // /etc/... is the display form of an Etc target even when --etc-root remaps it.
    if let Ok(rel) = expanded.strip_prefix("/etc") {
        if roots.etc != Path::new("/etc") {
            return Ok(Target::Etc(rel.to_path_buf()));
        }
    }
    let expanded = canonical(&expanded);
    if let Ok(rel) = expanded.strip_prefix(canonical(&roots.home)) {
        return Ok(Target::Home(rel.to_path_buf()));
    }
    if let Ok(rel) = expanded.strip_prefix(canonical(&roots.etc)) {
        return Ok(Target::Etc(rel.to_path_buf()));
    }
    bail!("not managed: {path}")
}

fn force_from(args: &UpArgs, roots: &Roots) -> Result<Force> {
    if args.force.is_empty() {
        return Ok(Force::None);
    }
    if args.force.iter().any(String::is_empty) {
        return Ok(Force::All);
    }
    let mut set = BTreeSet::new();
    for p in &args.force {
        set.insert(resolve_target(p, roots)?);
    }
    Ok(Force::Only(set))
}

fn plan_for(
    items: &[Desired],
    roots: &Roots,
    blobs: &Store,
    state: &State,
    opts: &Options,
) -> Result<Vec<Plan>> {
    let disk = RealDisk {
        roots: roots.clone(),
    };
    reconcile(items, state, &disk, blobs, opts)
}

fn up(args: &UpArgs, roots: &Roots, etc_root: Option<&Path>) -> Result<i32> {
    // SAFETY-free: geteuid has no preconditions.
    if !args.etc_only && unsafe { libc::geteuid() } == 0 && std::env::var_os("SUDO_USER").is_some()
    {
        bail!("run rig up without sudo; it escalates by itself");
    }
    let source = args.repo.as_deref().or(args.source.as_deref());
    let session = open(source, args.host.as_deref(), !args.etc_only)?;
    let opts = Options {
        force: force_from(args, roots)?,
        adopt: args.adopt,
        prune: args.prune,
    };
    let items = session.desired()?;
    let mut report = Report::live(args.verbose);
    if args.dry_run {
        println!("(dry run)");
    }

    if args.etc_only {
        let (store, mut state) = Store::open(&etc_state_dir(etc_root))?;
        apply_phase(&items, roots, &store, &mut state, true, args, &mut report)?;
        report.finish();
        return Ok(report.exit());
    }

    let (home_store, mut home_state) = Store::open(&home_state_dir()?)?;
    apply_phase(
        &items,
        roots,
        &home_store,
        &mut home_state,
        false,
        args,
        &mut report,
    )?;

    let (etc_store, etc_state) = Store::open(&etc_state_dir(etc_root))?;
    let etc: Vec<Plan> = plan_for(&items, roots, &etc_store, &etc_state, &opts)?
        .into_iter()
        .filter(|p| p.target.is_etc())
        .collect();
    let (actionable, informational): (Vec<Plan>, Vec<Plan>) =
        etc.into_iter().partition(changes_disk_or_state);

    // Rows rig can report without root belong in the parent's own summary.
    for p in &informational {
        report.push(Row::from_plan(p));
    }
    if args.dry_run {
        for p in &actionable {
            report.push(dry_row(p));
        }
    }
    let mut exit = 0;
    if !actionable.is_empty() && !args.dry_run {
        if is_root() {
            let mut state = etc_state;
            apply(
                &actionable,
                roots,
                &etc_store,
                &mut state,
                false,
                &mut report,
            )?;
        } else {
            let code = escalate(args, &session, etc_root, actionable.len(), &mut report)?;
            exit = exit.max(code);
            // The child printed its own rows; keep them here only so hooks see the writes.
            if code < 2 {
                for p in &actionable {
                    report.push(Row::from_plan(p).quietly());
                }
            }
        }
    }

    sync_machine(
        args,
        &session,
        &opts,
        roots,
        &home_store,
        &mut home_state,
        &mut report,
    )?;
    report.finish();
    Ok(exit.max(report.exit()))
}

/// Settings, packages and hooks: everything after the files are on disk.
fn sync_machine(
    args: &UpArgs,
    session: &Session,
    opts: &Options,
    roots: &Roots,
    store: &Store,
    state: &mut State,
    report: &mut Report,
) -> Result<()> {
    let policy = Policy {
        no_sudo: args.no_sudo,
    };
    let sel = session.selection()?;
    settings::sync(
        &settings::providers(),
        &sel,
        state,
        settings::Options {
            policy,
            dry_run: args.dry_run,
            force: matches!(opts.force, Force::All),
        },
        report,
    )?;
    if !args.dry_run {
        store.save(state)?;
    }
    let before = report.rows.len();
    packages::sync(
        &sel,
        &session.os,
        state,
        store,
        packages::Options {
            confirm: Confirm { yes: args.yes },
            policy,
            dry_run: args.dry_run,
        },
        report,
    )?;
    let written = written_by_module(&report.rows[..before], roots);
    let visible = report.rows.iter().any(|r| !r.quiet);
    let runs = run_hooks(&sel, &written, &roots.home, args.dry_run);
    for row in hooks::rows(&runs, visible) {
        report.push(row);
    }
    Ok(())
}

/// The absolute paths `up` wrote, grouped by owning module, for `RIG_CHANGED`.
fn written_by_module(rows: &[Row], roots: &Roots) -> BTreeMap<String, Vec<PathBuf>> {
    let mut out: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for row in rows {
        let (Some(module), Ok(target)) = (row.module.as_ref(), row.target.parse::<Target>()) else {
            continue;
        };
        if row.wrote {
            out.entry(module.clone())
                .or_default()
                .push(target.resolve(roots));
        }
    }
    out
}

/// Everything that needs write access to `/etc` or `/var/lib/rig`.
fn changes_disk_or_state(plan: &Plan) -> bool {
    !matches!(plan.action, Action::Nothing | Action::Forget)
}

fn apply_phase(
    items: &[Desired],
    roots: &Roots,
    store: &Store,
    state: &mut State,
    etc: bool,
    args: &UpArgs,
    report: &mut Report,
) -> Result<()> {
    let opts = Options {
        force: force_from(args, roots)?,
        adopt: args.adopt,
        prune: args.prune,
    };
    let plans: Vec<Plan> = plan_for(items, roots, store, state, &opts)?
        .into_iter()
        .filter(|p| p.target.is_etc() == etc)
        .collect();
    apply(&plans, roots, store, state, args.dry_run, report)
}

fn escalate(
    args: &UpArgs,
    session: &Session,
    etc_root: Option<&Path>,
    count: usize,
    report: &mut Report,
) -> Result<i32> {
    use std::io::IsTerminal;
    let hint = format!(
        "/etc ({count} targets, run: sudo rig up --etc-only --host {} --repo {})",
        session.host,
        session.repo.root.display()
    );
    let batch = !std::io::stdin().is_terminal();
    // Probe first, so sudo's own refusal is never confused with the child's exit code.
    let refused = || -> Result<bool> {
        Ok(batch
            && !std::process::Command::new("sudo")
                .args(["-n", "--", "true"])
                .status()
                .context("running sudo")?
                .success())
    };
    if args.no_sudo || refused()? {
        report.push(Row::new("needs-root", &hint).exit(1));
        return Ok(1);
    }
    eprintln!("escalating: {count} /etc targets need root");
    let mut cmd = std::process::Command::new("sudo");
    if batch {
        cmd.arg("-n");
    }
    cmd.arg("--")
        .arg(std::env::current_exe()?)
        .arg("up")
        .arg("--etc-only");
    if let Some(r) = etc_root {
        cmd.arg("--etc-root").arg(r);
    }
    cmd.arg("--host")
        .arg(&session.host)
        .arg("--repo")
        .arg(&session.repo.root);
    for f in &args.force {
        cmd.arg("--force");
        if !f.is_empty() {
            cmd.arg(f);
        }
    }
    for (flag, on) in [
        ("--adopt", args.adopt),
        ("--prune", args.prune),
        ("-y", args.yes),
        ("-v", args.verbose),
    ] {
        if on {
            cmd.arg(flag);
        }
    }
    Ok(cmd.status().context("running sudo")?.code().unwrap_or(2))
}

fn status(roots: &Roots, host: Option<&str>, verbose: bool) -> Result<i32> {
    let session = open(None, host, false)?;
    let items = session.desired()?;
    let opts = Options::default();
    let mut rows = Vec::new();
    for (dir, etc) in state_dirs(roots)? {
        let (store, state) = Store::open(&dir)?;
        for p in plan_for(&items, roots, &store, &state, &opts)? {
            if p.target.is_etc() == etc {
                rows.push(Row::from_plan(&p));
            }
        }
    }
    rows.sort_by(|a, b| a.target.cmp(&b.target));
    let mut report = Report::live(verbose);
    for row in rows {
        report.push(row);
    }
    let (_, state) = Store::open(&home_state_dir()?)?;
    settings::status(
        &settings::providers(),
        &session.selection()?,
        &state,
        Policy::default(),
        &mut report,
    )?;
    report.finish();
    Ok(report.exit())
}

/// The state directory that owns this target.
fn state_dir_for(target: &Target, roots: &Roots) -> Result<PathBuf> {
    Ok(if target.is_etc() {
        etc_state_dir(roots_etc_root(roots).as_deref())
    } else {
        home_state_dir()?
    })
}

/// The home and etc state directories, each paired with the target kind it owns.
fn state_dirs(roots: &Roots) -> Result<[(PathBuf, bool); 2]> {
    Ok([
        (home_state_dir()?, false),
        (etc_state_dir(roots_etc_root(roots).as_deref()), true),
    ])
}

/// `Roots` remembers the remapped `/etc`; recover the test root from it.
fn roots_etc_root(roots: &Roots) -> Option<PathBuf> {
    (roots.etc != Path::new("/etc")).then(|| roots.etc.parent().unwrap().to_path_buf())
}

fn label_patch(old: &[u8], new: &[u8], from: &str, to: &str) -> Vec<u8> {
    let mut opts = diffy::DiffOptions::default();
    opts.set_original_filename(from.to_string());
    opts.set_modified_filename(to.to_string());
    opts.create_patch_bytes(old, new).to_bytes()
}

fn diff(roots: &Roots, path: Option<&str>, host: Option<&str>) -> Result<i32> {
    let session = open(None, host, false)?;
    let only = path.map(|p| resolve_target(p, roots)).transpose()?;
    let items = session.desired()?;
    if let Some(t) = &only {
        if !items.iter().any(|d| d.target == *t) {
            bail!("not managed: {}", path.unwrap_or_default());
        }
    }
    let disk = RealDisk {
        roots: roots.clone(),
    };
    let mut exit = 0;
    for (dir, etc) in state_dirs(roots)? {
        let (store, state) = Store::open(&dir)?;
        for p in reconcile(&items, &state, &disk, &store, &Options::default())? {
            if p.target.is_etc() != etc || only.as_ref().is_some_and(|t| *t != p.target) {
                continue;
            }
            let Some(d) = items.iter().find(|d| d.target == p.target) else {
                continue;
            };
            let on_disk = disk.read(&p.target)?.unwrap_or_default();
            let body = match (p.outcome, &p.action) {
                (Outcome::Updated | Outcome::Conflict(_), Action::Write { content, .. }) => {
                    label_patch(&on_disk, content, "disk", "repo")
                }
                (Outcome::Conflict(_), Action::Conflict { marked }) => marked.clone(),
                (Outcome::Conflict(_), Action::Nothing) => {
                    label_patch(&on_disk, &d.content, "disk", "repo")
                }
                (Outcome::Edited, _) => label_patch(&d.content, &on_disk, "repo", "disk"),
                (Outcome::Merged, Action::Write { content, .. }) => {
                    label_patch(&on_disk, content, "disk", "merged")
                }
                _ => continue,
            };
            exit = 1;
            println!("{}", p.target);
            std::io::stdout().write_all(&body)?;
        }
    }
    if only.is_none() {
        let (_, state) = Store::open(&home_state_dir()?)?;
        let (items, _) = settings::plan(
            &settings::providers(),
            &session.selection()?,
            &state,
            Policy::default(),
        )?;
        for item in items {
            if !matches!(
                item.outcome,
                settings::Outcome::Ok | settings::Outcome::Adopted
            ) {
                exit = 1;
                println!(
                    "{}: {} -> {}",
                    item.label(),
                    item.machine.as_deref().unwrap_or("unset"),
                    item.desired
                );
            }
        }
    }
    Ok(exit)
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
    warnings.extend(packages::mise::path_warning());
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    println!(
        "{} modules, {} hosts, {} defaults dirs",
        repo.modules.len(),
        repo.hosts.len(),
        repo.defaults.len()
    );
    for line in packages::doctor_lines(Policy::default()) {
        println!("{line}");
    }
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

// Absorb.

/// What rig last wrote and what is on disk now.
struct Sides {
    base: Vec<u8>,
    disk: Vec<u8>,
}

fn sides(
    target: &Target,
    desired: &Desired,
    store: &Store,
    state: &State,
    disk: &RealDisk,
) -> Result<Sides> {
    let Some(entry) = state.entries.get(target) else {
        bail!("not managed: use rig adopt {target} --module <m>");
    };
    let Some(on_disk) = disk.read(target)? else {
        bail!("file is missing on disk: {target}");
    };
    let base = store
        .blob(&entry.hash)?
        .with_context(|| format!("state blob missing for {target}; run rig up --force {target}"))?;
    let k = Hash::of(&on_disk);
    let d = Hash::of(&desired.content);
    if k == entry.hash {
        if d == entry.hash {
            bail!("nothing to absorb: {target} is unchanged");
        }
        bail!("repo changed too: run rig up first, then absorb");
    }
    if d != entry.hash {
        if store.conflicts()?.contains(target) {
            bail!("conflict: run rig resolve first");
        }
        bail!("repo changed too: run rig up first, then absorb");
    }
    Ok(Sides {
        base,
        disk: on_disk,
    })
}

fn top_layer(target: &Target, ls: &[Layer]) -> Result<(TopLayer, usize)> {
    let idx = ls
        .iter()
        .rposition(|l| l.source.module().is_some())
        .with_context(|| format!("{target} has no module layer"))?;
    let file = ls[idx].source.path().to_path_buf();
    let append = ls[idx].append;
    let lower = if idx == 0 {
        None
    } else {
        Some(Lower {
            file: ls[idx - 1].source.path().to_path_buf(),
            content: compose(target, &ls[..idx])?.content,
        })
    };
    Ok((
        TopLayer {
            file,
            content: ls[idx].content.clone(),
            append,
            lower,
        },
        idx,
    ))
}

/// Rewrites the layer file, then checks that composing again reproduces the disk.
///
/// Returns the edit and whether the disk had to be rewritten in the layer's formatting.
fn absorb_one(
    target: &Target,
    desired: &Desired,
    all_layers: &BTreeMap<Target, Vec<Layer>>,
    store: &Store,
    state: &mut State,
    roots: &Roots,
) -> Result<(AbsorbEdit, bool)> {
    let disk = RealDisk {
        roots: roots.clone(),
    };
    let s = sides(target, desired, store, state, &disk)?;
    let ls = &all_layers[target];
    let (top, idx) = top_layer(target, ls)?;
    let previous = top.content.clone();
    let edit = absorb(desired.format, &top, &s.base, &s.disk)?;

    write_atomic(&edit.layer_file, &edit.new_content, ls[idx].mode)?;
    let mut updated = ls.clone();
    updated[idx].content.clone_from(&edit.new_content);
    let recomposed = compose(target, &updated)?;

    if recomposed.content == s.disk {
        record_absorbed(target, desired, &recomposed.content, store, state)?;
        return Ok((edit, false));
    }
    if equivalent(desired.format, &recomposed.content, &s.disk) {
        write_atomic(&target.resolve(roots), &recomposed.content, desired.mode)?;
        record_absorbed(target, desired, &recomposed.content, store, state)?;
        return Ok((edit, true));
    }
    write_atomic(&edit.layer_file, &previous, ls[idx].mode)?;
    let base = store
        .root()
        .join("absorb-failed")
        .join(target.module_path());
    let dump = |suffix: &str| {
        let mut name = base.file_name().unwrap().to_os_string();
        name.push(suffix);
        base.with_file_name(name)
    };
    write_atomic(&dump(".intended"), &s.disk, 0o644)?;
    write_atomic(&dump(".actual"), &recomposed.content, 0o644)?;
    bail!(
        "absorb did not round trip for {target}; see {} and {}",
        dump(".intended").display(),
        dump(".actual").display()
    )
}

fn record_absorbed(
    target: &Target,
    desired: &Desired,
    content: &[u8],
    store: &Store,
    state: &mut State,
) -> Result<()> {
    let hash = store.put_blob(content)?;
    state.entries.insert(
        target.clone(),
        Entry {
            hash,
            module: desired.module.clone(),
            mode: desired.mode,
        },
    );
    store.save(state)
}

fn absorb_line(
    session: &Session,
    module: &str,
    edit: &AbsorbEdit,
    normalized: bool,
    verbose: bool,
) -> String {
    let root = session.repo.root.join("modules").join(module);
    let rel = edit.layer_file.strip_prefix(&root).map_or_else(
        |_| session.rel(&edit.layer_file),
        |p| p.display().to_string(),
    );
    let mut line = format!("  {module}: {rel}  {}", summary(&edit.changes));
    if normalized {
        line.push_str("  normalized");
    }
    if verbose && edit.changes.len() > 4 {
        for c in &edit.changes {
            use std::fmt::Write as _;
            let _ = write!(line, "\n    {}", c.render());
        }
    }
    line
}

fn absorb_cmd(
    roots: &Roots,
    path: Option<&str>,
    all: bool,
    host: Option<&str>,
    verbose: bool,
) -> Result<i32> {
    let session = open(None, host, false)?;
    let items = session.desired()?;
    let all_layers = session.layers()?;
    let sel = session.selection()?;
    let mut stores: Vec<(bool, Store, State)> = Vec::new();
    for (dir, etc) in state_dirs(roots)? {
        let (store, state) = Store::open(&dir)?;
        stores.push((etc, store, state));
    }

    let chosen: Vec<&Desired> = if all {
        let mut out = Vec::new();
        for (etc, store, state) in &stores {
            out.extend(
                edited_targets(&items, store, state, roots)?
                    .into_iter()
                    .filter(|d| d.target.is_etc() == *etc),
            );
        }
        out
    } else {
        let p = path.context("rig absorb needs a path or --all")?;
        let target = resolve_target(p, roots)?;
        vec![items
            .iter()
            .find(|d| d.target == target)
            .with_context(|| format!("not managed: {p}; use rig adopt {p} --module <m>"))?]
    };

    let mut report = Report::live(verbose);
    for d in chosen {
        if all && sel.module(&d.module).map(|m| m.sync) == Some(Sync::Manual) {
            report.push(
                Row::new("skipped", &d.target.to_string())
                    .note("module is sync = manual")
                    .exit(1),
            );
            continue;
        }
        let (_, store, state) = stores
            .iter_mut()
            .find(|(etc, _, _)| *etc == d.target.is_etc())
            .context("no state store for this target")?;
        match absorb_one(&d.target, d, &all_layers, store, state, roots) {
            Ok((edit, normalized)) => {
                println!(
                    "{}",
                    absorb_line(&session, &d.module, &edit, normalized, verbose)
                );
            }
            Err(e) if all => {
                report.push(
                    Row::new("error", &d.target.to_string())
                        .note(&format!("{e:#}"))
                        .exit(1),
                );
            }
            Err(e) => return Err(e),
        }
    }
    Ok(report.exit())
}

/// Targets in cell 7: the disk moved, the repo did not.
fn edited_targets<'a>(
    items: &'a [Desired],
    store: &Store,
    state: &State,
    roots: &Roots,
) -> Result<Vec<&'a Desired>> {
    let disk = RealDisk {
        roots: roots.clone(),
    };
    let plans = reconcile(items, state, &disk, store, &Options::default())?;
    Ok(plans
        .iter()
        .filter(|p| p.outcome == Outcome::Edited)
        .filter_map(|p| items.iter().find(|d| d.target == p.target))
        .collect())
}

// Adopt.

fn adopt(
    roots: &Roots,
    path: &str,
    module: &str,
    host_variant: bool,
    os_variant: bool,
    host: Option<&str>,
) -> Result<i32> {
    let session = open(None, host, false)?;
    let target = resolve_target(path, roots)?;
    let source = target.resolve(roots);
    if source.is_dir() {
        bail!("directories are not supported; adopt files one at a time");
    }
    if !source.is_file() {
        bail!("no such file: {path}");
    }
    let items = session.desired()?;
    if let Some(d) = items.iter().find(|d| d.target == target) {
        bail!("already managed by {}: use rig absorb", d.module);
    }
    let sel = session.selection()?;
    if sel.module(module).is_none() {
        if session.repo.modules.contains_key(module) {
            bail!(
                "module {module} is not active for host {}; add it to hosts/{}.toml first",
                session.host,
                session.host
            );
        }
        bail!("no such module {module}; create modules/{module}/");
    }

    let mut name = target
        .rel()
        .file_name()
        .context("target has no file name")?
        .to_string_lossy()
        .into_owned();
    if host_variant {
        name.push('@');
        name.push_str(&session.host);
    } else if os_variant {
        name.push('@');
        name.push_str(
            session
                .os
                .distro
                .as_deref()
                .unwrap_or(if session.os.matches("macos") {
                    "macos"
                } else {
                    "linux"
                }),
        );
    }
    let dest = session
        .repo
        .root
        .join("modules")
        .join(module)
        .join(target.module_path())
        .with_file_name(name);
    if dest.exists() {
        bail!("{} already exists", session.rel(&dest));
    }
    let content = std::fs::read(&source).with_context(|| format!("reading {path}"))?;
    let mode = std::fs::metadata(&source)?.permissions().mode() & 0o777;
    write_atomic(&dest, &content, mode)?;

    let (store, mut state) = Store::open(&state_dir_for(&target, roots)?)?;
    let hash = store.put_blob(&content)?;
    state.entries.insert(
        target.clone(),
        Entry {
            hash,
            module: module.to_string(),
            mode,
        },
    );
    store.save(&state)?;

    let mut report = Report::live(false);
    report.push(
        Row::new("adopted", &target.to_string())
            .module(module)
            .note(&format!("-> {}", session.rel(&dest))),
    );
    Ok(0)
}

// Resolve.

fn has_markers(content: &[u8]) -> bool {
    String::from_utf8_lossy(content).lines().any(|l| {
        ["<<<<<<<", "|||||||", "=======", ">>>>>>>"]
            .iter()
            .any(|m| l.starts_with(m))
    })
}

fn run_editor(path: &Path) -> Result<bool> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("sh")
        .arg(path)
        .status()
        .context("running the editor")?;
    Ok(status.success())
}

fn resolve(
    roots: &Roots,
    path: Option<&str>,
    host: Option<&str>,
    etc_only: bool,
    repo: Option<&Path>,
    etc_root: Option<&Path>,
) -> Result<i32> {
    let session = open(repo, host, false)?;
    let items = session.desired()?;
    let only = path.map(|p| resolve_target(p, roots)).transpose()?;
    let mut exit = 0;
    let mut report = Report::live(false);
    let mut pending_etc = 0;

    for (dir, etc) in state_dirs(roots)? {
        if etc_only && !etc {
            continue;
        }
        let (store, mut state) = Store::open(&dir)?;
        for target in store.conflicts()? {
            if target.is_etc() != etc || only.as_ref().is_some_and(|t| *t != target) {
                continue;
            }
            if etc && !etc_only && !is_root() {
                pending_etc += 1;
                continue;
            }
            let Some(d) = items.iter().find(|d| d.target == target) else {
                report.push(
                    Row::new("conflict", &target.to_string())
                        .note("no longer in the repo; delete the conflict file by hand")
                        .exit(1),
                );
                continue;
            };
            let file = store.root().join("conflicts").join(target.module_path());
            if !run_editor(&file)? {
                report.push(
                    Row::new("conflict", &target.to_string())
                        .note("editor failed")
                        .exit(1),
                );
                continue;
            }
            let content = std::fs::read(&file)?;
            if has_markers(&content) {
                report.push(
                    Row::new("conflict", &target.to_string())
                        .note("markers remain, left in place")
                        .exit(1),
                );
                continue;
            }
            write_atomic(&target.resolve(roots), &content, d.mode)?;
            record_absorbed(&target, d, &d.content, &store, &mut state)?;
            store.clear_conflict(&target)?;
            report.push(
                Row::new("resolved", &target.to_string())
                    .module(&d.module)
                    .note("now edited, run rig absorb"),
            );
        }
    }
    if pending_etc > 0 {
        exit = exit.max(escalate_resolve(&session, path, etc_root)?);
    }
    report.finish();
    Ok(exit.max(report.exit()))
}

fn escalate_resolve(session: &Session, path: Option<&str>, etc_root: Option<&Path>) -> Result<i32> {
    eprintln!("escalating: /etc conflicts need root");
    let mut cmd = std::process::Command::new("sudo");
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        cmd.arg("-n");
    }
    cmd.arg("--")
        .arg(std::env::current_exe()?)
        .arg("resolve")
        .arg("--etc-only");
    if let Some(r) = etc_root {
        cmd.arg("--etc-root").arg(r);
    }
    cmd.arg("--host")
        .arg(&session.host)
        .arg("--repo")
        .arg(&session.repo.root);
    if let Some(p) = path {
        cmd.arg(p);
    }
    Ok(cmd.status().context("running sudo")?.code().unwrap_or(2))
}

// Init.

fn init(dir: Option<&Path>, host: Option<&str>) -> Result<i32> {
    let root = match dir {
        Some(d) => d.to_path_buf(),
        None => home()?.join("dotfiles"),
    };
    std::fs::create_dir_all(&root)?;
    let host = host.map_or_else(hostname, ToString::to_string);
    let mut created = Vec::new();
    let mut put = |rel: &str, content: &str| -> Result<()> {
        let path = root.join(rel);
        if path.exists() {
            return Ok(());
        }
        write_atomic(&path, content.as_bytes(), 0o644)?;
        created.push(rel.to_string());
        Ok(())
    };
    let fresh = !root.join("rig.toml").exists();
    if fresh {
        put("rig.toml", "defaults = [\"/usr/share/defaults\"]\n")?;
        put("modules/.keep", "")?;
        put(".gitignore", "")?;
    }
    put(&format!("hosts/{host}.toml"), "modules = []\n")?;

    if !root.join(".git").exists() {
        let status = std::process::Command::new("git")
            .arg("init")
            .arg("--quiet")
            .arg(&root)
            .status()
            .context("running git init")?;
        if !status.success() {
            bail!("git init failed in {}", root.display());
        }
    }
    let cfg = config_dir()?;
    std::fs::create_dir_all(&cfg)?;
    let abs = root.canonicalize()?;
    std::fs::write(cfg.join("repo"), format!("{}\n", abs.display()))?;
    std::fs::write(cfg.join("host"), format!("{host}\n"))?;
    for c in &created {
        println!("created {}", root.join(c).display());
    }
    println!("host {host} registered");
    Ok(0)
}
