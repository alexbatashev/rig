//! Command line entry points.

use crate::apply::{apply, dry_row, RealDisk};
use crate::compose::{desired, Desired};
use crate::reconcile::{reconcile, Action, Disk, Force, Options, Outcome, Plan};
use crate::repo::{load, select, Os, Repo, Roots, Target, KNOWN_OS_TAGS};
use crate::report::{Report, Row};
use crate::state::{State, Store};
use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::collections::BTreeSet;
use std::io::Write;
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
    fn desired(&self) -> Result<Vec<Desired>> {
        let sel = select(&self.repo, &self.host, &self.os)?;
        desired(&self.repo, &sel, &self.os)
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

fn print_report(report: &Report, verbose: bool) -> Result<()> {
    let mut out = std::io::stdout().lock();
    report.print(&mut out, verbose)?;
    out.flush()?;
    Ok(())
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
    if args.dry_run {
        println!("(dry run)");
    }

    if args.etc_only {
        return apply_phase(&items, roots, &etc_state_dir(etc_root), true, args);
    }

    let mut exit = apply_phase(&items, roots, &home_state_dir()?, false, args)?;

    let (etc_store, etc_state) = Store::open(&etc_state_dir(etc_root))?;
    let etc: Vec<Plan> = plan_for(&items, roots, &etc_store, &etc_state, &opts)?
        .into_iter()
        .filter(|p| p.target.is_etc())
        .collect();
    let (actionable, informational): (Vec<Plan>, Vec<Plan>) =
        etc.into_iter().partition(changes_disk_or_state);

    // Rows rig can report without root are printed by the parent, whatever happens next.
    let mut noted = Report::default();
    for p in &informational {
        noted.push(Row::from_plan(p));
    }
    if !noted.rows.is_empty() {
        print_report(&noted, args.verbose)?;
        exit = exit.max(noted.exit());
    }
    if actionable.is_empty() {
        return Ok(exit);
    }
    if args.dry_run {
        let mut r = Report::default();
        for p in &actionable {
            r.push(dry_row(p));
        }
        print_report(&r, args.verbose)?;
        return Ok(exit.max(r.exit()));
    }
    // SAFETY-free: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        let mut state = etc_state;
        let r = apply(&actionable, roots, &etc_store, &mut state, false)?;
        print_report(&r, args.verbose)?;
        return Ok(exit.max(r.exit()));
    }
    Ok(exit.max(escalate(args, &session, etc_root, actionable.len())?))
}

/// Everything that needs write access to `/etc` or `/var/lib/rig`.
fn changes_disk_or_state(plan: &Plan) -> bool {
    !matches!(plan.action, Action::Nothing | Action::Forget)
}

fn apply_phase(
    items: &[Desired],
    roots: &Roots,
    state_dir: &Path,
    etc: bool,
    args: &UpArgs,
) -> Result<i32> {
    let opts = Options {
        force: force_from(args, roots)?,
        adopt: args.adopt,
        prune: args.prune,
    };
    let (store, mut state) = Store::open(state_dir)?;
    let plans: Vec<Plan> = plan_for(items, roots, &store, &state, &opts)?
        .into_iter()
        .filter(|p| p.target.is_etc() == etc)
        .collect();
    let report = apply(&plans, roots, &store, &mut state, args.dry_run)?;
    print_report(&report, args.verbose)?;
    Ok(report.exit())
}

fn escalate(
    args: &UpArgs,
    session: &Session,
    etc_root: Option<&Path>,
    count: usize,
) -> Result<i32> {
    use std::io::IsTerminal;
    let hint = format!(
        "/etc ({count} targets, run: sudo rig up --etc-only --host {} --repo {})",
        session.host,
        session.repo.root.display()
    );
    let needs_root = || -> Result<i32> {
        let mut r = Report::default();
        r.push(Row::new("needs-root", &hint).exit(1));
        print_report(&r, args.verbose)?;
        Ok(1)
    };
    if args.no_sudo {
        return needs_root();
    }
    let batch = !std::io::stdin().is_terminal();
    // Probe first, so sudo's own refusal is never confused with the child's exit code.
    if batch
        && !std::process::Command::new("sudo")
            .args(["-n", "--", "true"])
            .status()
            .context("running sudo")?
            .success()
    {
        return needs_root();
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
    let mut report = Report::default();
    for (dir, etc) in state_dirs(roots)? {
        let (store, state) = Store::open(&dir)?;
        for p in plan_for(&items, roots, &store, &state, &opts)? {
            if p.target.is_etc() == etc {
                report.push(Row::from_plan(&p));
            }
        }
    }
    report.rows.sort_by(|a, b| a.target.cmp(&b.target));
    print_report(&report, verbose)?;
    Ok(report.exit())
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
