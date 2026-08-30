//! The on-disk config repository: parsing, host selection and OS detection.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Tags that always name an OS rather than a host.
pub const KNOWN_OS_TAGS: &[&str] = &["linux", "macos", "arch", "ubuntu", "debian", "keel"];

/// A path rig manages, relative to its root.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    Home(PathBuf),
    Etc(PathBuf),
}

impl Target {
    /// `home/.config/x` becomes `Home(".config/x")`, `etc/x` becomes `Etc("x")`.
    ///
    /// # Errors
    /// When the first component is neither `home` nor `etc`.
    pub fn from_module_path(rel: &Path) -> Result<Target> {
        let mut it = rel.components();
        let first = it
            .next()
            .with_context(|| format!("empty module path {}", rel.display()))?;
        let rest: PathBuf = it.collect();
        match first.as_os_str().to_str() {
            Some("home") => Ok(Target::Home(rest)),
            Some("etc") => Ok(Target::Etc(rest)),
            _ => bail!("{}: module files live under home/ or etc/", rel.display()),
        }
    }

    #[must_use]
    pub fn rel(&self) -> &Path {
        match self {
            Target::Home(p) | Target::Etc(p) => p,
        }
    }

    /// The module-relative path, including the `home`/`etc` prefix.
    #[must_use]
    pub fn module_path(&self) -> PathBuf {
        match self {
            Target::Home(p) => Path::new("home").join(p),
            Target::Etc(p) => Path::new("etc").join(p),
        }
    }

    /// Absolute path on this machine.
    #[must_use]
    pub fn resolve(&self, roots: &Roots) -> PathBuf {
        match self {
            Target::Home(p) => roots.home.join(p),
            Target::Etc(p) => roots.etc.join(p),
        }
    }

    #[must_use]
    pub fn is_etc(&self) -> bool {
        matches!(self, Target::Etc(_))
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::Home(p) => write!(f, "~/{}", p.display()),
            Target::Etc(p) => write!(f, "/etc/{}", p.display()),
        }
    }
}

impl FromStr for Target {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Target> {
        if let Some(r) = s.strip_prefix("~/") {
            Ok(Target::Home(PathBuf::from(r)))
        } else if let Some(r) = s.strip_prefix("/etc/") {
            Ok(Target::Etc(PathBuf::from(r)))
        } else {
            bail!("not a target: {s}")
        }
    }
}

/// Where `Home` and `Etc` targets land. `etc` is `/etc` outside tests.
#[derive(Clone, Debug)]
pub struct Roots {
    pub home: PathBuf,
    pub etc: PathBuf,
}

impl Roots {
    #[must_use]
    pub fn new(home: PathBuf, etc_root: Option<&Path>) -> Roots {
        let etc = etc_root.map_or_else(|| PathBuf::from("/etc"), |r| r.join("etc"));
        Roots { home, etc }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Toml,
    Json,
    Ini,
    Text,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Linux,
    Macos,
}

#[derive(Clone, Debug)]
pub struct Os {
    pub family: Family,
    pub distro: Option<String>,
    pub like: Vec<String>,
}

impl Os {
    /// Reads `RIG_OS` if set, else `uname` plus `/etc/os-release`.
    #[must_use]
    pub fn detect() -> Os {
        if let Ok(spec) = std::env::var("RIG_OS") {
            return Os::parse(&spec);
        }
        if cfg!(target_os = "macos") {
            return Os {
                family: Family::Macos,
                distro: Some("macos".into()),
                like: Vec::new(),
            };
        }
        let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        let field = |k: &str| {
            release.lines().find_map(|l| {
                l.strip_prefix(k)
                    .map(|v| v.trim_matches('"').to_lowercase())
            })
        };
        Os {
            family: Family::Linux,
            distro: field("ID="),
            like: field("ID_LIKE=")
                .map(|v| v.split_whitespace().map(ToString::to_string).collect())
                .unwrap_or_default(),
        }
    }

    /// `linux:arch`, `linux:omarchy:arch`, or `macos`.
    #[must_use]
    pub fn parse(spec: &str) -> Os {
        let mut parts = spec.split(':');
        let family = match parts.next() {
            Some("macos") => Family::Macos,
            _ => Family::Linux,
        };
        let distro = parts.next().map(str::to_string).or(match family {
            Family::Macos => Some("macos".into()),
            Family::Linux => None,
        });
        let like = parts
            .next()
            .map(|l| l.split(',').map(ToString::to_string).collect())
            .unwrap_or_default();
        Os {
            family,
            distro,
            like,
        }
    }

    #[must_use]
    pub fn matches(&self, tag: &str) -> bool {
        match tag {
            "linux" => self.family == Family::Linux,
            "macos" => self.family == Family::Macos,
            _ => self.distro.as_deref() == Some(tag) || self.like.iter().any(|l| l == tag),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Arch,
    Ubuntu,
    Macos,
    Nix,
    Keel,
    Mise,
}

impl Backend {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Backend::Arch => "arch",
            Backend::Ubuntu => "ubuntu",
            Backend::Macos => "macos",
            Backend::Nix => "nix",
            Backend::Keel => "keel",
            Backend::Mise => "mise",
        }
    }
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookWhen {
    #[default]
    Changed,
    Always,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sync {
    #[default]
    Auto,
    Manual,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    pub after: String,
    #[serde(default)]
    pub when: HookWhen,
}

#[derive(Clone, Debug)]
pub struct ModuleFile {
    pub path: PathBuf,
    pub target: Target,
    pub variant: Option<String>,
    pub append: bool,
    pub mode: u32,
}

#[derive(Clone, Debug)]
pub struct Module {
    pub name: String,
    pub root: PathBuf,
    pub when_os: Option<Vec<String>>,
    pub packages: BTreeMap<Backend, Vec<String>>,
    pub hooks: Vec<Hook>,
    pub sync: Sync,
    pub files: Vec<ModuleFile>,
}

#[derive(Clone, Debug)]
pub struct Host {
    pub modules: Vec<String>,
}

/// An OS defaults directory: the lowest layer, laid out like a module.
#[derive(Clone, Debug)]
pub struct Defaults {
    pub root: PathBuf,
    pub files: Vec<ModuleFile>,
}

#[derive(Clone, Debug)]
pub struct Repo {
    pub root: PathBuf,
    pub defaults: Vec<Defaults>,
    pub missing_defaults: Vec<PathBuf>,
    pub hosts: BTreeMap<String, Host>,
    pub modules: BTreeMap<String, Module>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RigToml {
    #[serde(default)]
    defaults: Vec<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostToml {
    modules: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ModuleToml {
    #[serde(default)]
    when: Option<WhenToml>,
    #[serde(default)]
    sync: Sync,
    #[serde(default)]
    packages: BTreeMap<Backend, Vec<String>>,
    #[serde(default, rename = "hook")]
    hooks: Vec<Hook>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WhenToml {
    #[serde(default)]
    os: Option<Vec<String>>,
}

fn parse_toml<T: serde::de::DeserializeOwned>(path: &Path, root: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", rel_display(path, root)))?;
    toml::from_str(&text).map_err(|e| {
        let line = e
            .span()
            .map_or(1, |s| text[..s.start].matches('\n').count() + 1);
        anyhow::anyhow!("{}:{line}: {}", rel_display(path, root), e.message())
    })
}

fn rel_display(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Splits a file name into `(name, variant, append)`.
///
/// # Errors
/// When stripping the variant and `.append` suffix leaves nothing.
pub fn parse_file_name(name: &str) -> Result<(String, Option<String>, bool)> {
    let (stem, append) = match name.strip_suffix(".append") {
        Some(s) => (s, true),
        None => (name, false),
    };
    let valid = |t: &str| {
        !t.is_empty()
            && t.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
    };
    let (base, variant) = match stem.rfind('@') {
        Some(i) if valid(&stem[i + 1..]) => (&stem[..i], Some(stem[i + 1..].to_string())),
        _ => (stem, None),
    };
    if base.is_empty() {
        bail!("{name}: file name is empty once the variant tag is stripped");
    }
    Ok((base.to_string(), variant, append))
}

fn walk_tree(root: &Path, kind: &str, repo_root: &Path) -> Result<Vec<ModuleFile>> {
    let dir = root.join(kind);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(&dir)
        .follow_links(false)
        .sort_by_file_name()
    {
        let entry = entry.with_context(|| format!("walking {}", rel_display(&dir, repo_root)))?;
        if entry.path_is_symlink() {
            bail!(
                "{}: symlinks are not supported, commit the file",
                rel_display(entry.path(), repo_root)
            );
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name == ".DS_Store" {
            continue;
        }
        let (base, variant, append) =
            parse_file_name(&name).with_context(|| rel_display(path, repo_root))?;
        let rel = path.strip_prefix(root).unwrap().with_file_name(base);
        let target =
            Target::from_module_path(&rel).with_context(|| rel_display(path, repo_root))?;
        let mode = std::fs::metadata(path)
            .with_context(|| format!("stat {}", rel_display(path, repo_root)))?
            .permissions()
            .mode()
            & 0o777;
        out.push(ModuleFile {
            path: path.to_path_buf(),
            target,
            variant,
            append,
            mode,
        });
    }
    Ok(out)
}

fn load_tree(root: &Path, repo_root: &Path) -> Result<Vec<ModuleFile>> {
    let mut files = walk_tree(root, "home", repo_root)?;
    files.extend(walk_tree(root, "etc", repo_root)?);
    Ok(files)
}

fn load_hosts(root: &Path, modules: &BTreeMap<String, Module>) -> Result<BTreeMap<String, Host>> {
    let mut hosts = BTreeMap::new();
    let hosts_dir = root.join("hosts");
    if hosts_dir.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&hosts_dir)
            .with_context(|| format!("reading {}", hosts_dir.display()))?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::result::Result<_, _>>()?;
        entries.sort();
        for path in entries {
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let os_tag = KNOWN_OS_TAGS.contains(&name.as_str())
                || modules
                    .values()
                    .filter_map(|m| m.when_os.as_ref())
                    .any(|tags| tags.contains(&name));
            if os_tag {
                bail!("hosts/{name}.toml: '{name}' names an OS, pick another host name");
            }
            let cfg: HostToml = parse_toml(&path, root)?;
            for (i, m) in cfg.modules.iter().enumerate() {
                if !modules.contains_key(m) {
                    bail!("hosts/{name}.toml: no such module '{m}'");
                }
                if cfg.modules[..i].contains(m) {
                    bail!("hosts/{name}.toml: module '{m}' listed twice");
                }
            }
            hosts.insert(
                name,
                Host {
                    modules: cfg.modules,
                },
            );
        }
    }

    Ok(hosts)
}

/// Loads a config repository.
///
/// # Errors
/// On any parse error, symlink, unknown module reference or reserved host name.
///
/// # Panics
/// Never; every path walked comes from `read_dir` and has a file name.
pub fn load(root: &Path) -> Result<Repo> {
    let root = root
        .canonicalize()
        .with_context(|| format!("no such repo: {}", root.display()))?;
    let rig_toml = root.join("rig.toml");
    let cfg: RigToml = if rig_toml.exists() {
        parse_toml(&rig_toml, &root)?
    } else {
        RigToml::default()
    };

    let mut defaults = Vec::new();
    let mut missing_defaults = Vec::new();
    for d in cfg.defaults {
        let dir = if d.is_absolute() { d } else { root.join(&d) };
        if dir.is_dir() {
            let files = load_tree(&dir, &root)?;
            defaults.push(Defaults { root: dir, files });
        } else {
            missing_defaults.push(dir);
        }
    }

    let mut modules = BTreeMap::new();
    let modules_dir = root.join("modules");
    if modules_dir.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&modules_dir)
            .with_context(|| format!("reading {}", modules_dir.display()))?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::result::Result<_, _>>()?;
        entries.sort();
        for dir in entries {
            if !dir.is_dir() {
                continue;
            }
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            let toml_path = dir.join("module.toml");
            let has_toml = toml_path.exists();
            let cfg: ModuleToml = if has_toml {
                parse_toml(&toml_path, &root)?
            } else {
                ModuleToml::default()
            };
            let empty = std::fs::read_dir(&dir)?.next().is_none();
            if !empty && !has_toml && !dir.join("home").is_dir() && !dir.join("etc").is_dir() {
                bail!("modules/{name}: no home/, no etc/ and no module.toml");
            }
            modules.insert(
                name.clone(),
                Module {
                    name,
                    root: dir.clone(),
                    when_os: cfg.when.and_then(|w| w.os),
                    packages: cfg.packages,
                    hooks: cfg.hooks,
                    sync: cfg.sync,
                    files: load_tree(&dir, &root)?,
                },
            );
        }
    }

    let hosts = load_hosts(&root, &modules)?;

    Ok(Repo {
        root,
        defaults,
        missing_defaults,
        hosts,
        modules,
    })
}

/// The active modules for a host, in the order the host lists them.
#[derive(Debug)]
pub struct Selection<'a> {
    pub host: String,
    pub modules: Vec<&'a Module>,
}

impl Selection<'_> {
    #[must_use]
    pub fn module(&self, name: &str) -> Option<&Module> {
        self.modules.iter().copied().find(|m| m.name == name)
    }
}

/// Picks the modules a host runs on this OS.
///
/// # Errors
/// When the host is not in the repo.
///
/// # Panics
/// Never; `load` rejects hosts that name a module the repo lacks.
pub fn select<'a>(repo: &'a Repo, host: &str, os: &Os) -> Result<Selection<'a>> {
    let Some(h) = repo.hosts.get(host) else {
        let known: Vec<&str> = repo.hosts.keys().map(String::as_str).collect();
        bail!("unknown host '{host}'; repo has: {}", known.join(", "));
    };
    let modules = h
        .modules
        .iter()
        .map(|n| &repo.modules[n])
        .filter(|m| {
            m.when_os
                .as_ref()
                .is_none_or(|tags| tags.iter().any(|t| os.matches(t)))
        })
        .collect();
    Ok(Selection {
        host: host.to_string(),
        modules,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::{desired, layers};

    fn build(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        for (rel, content) in files {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, content).unwrap();
        }
        dir
    }

    fn one_module(extra: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut v = vec![
            (
                "hosts/desktop.toml".to_string(),
                "modules = [\"m\"]\n".to_string(),
            ),
            (
                "hosts/macbook.toml".to_string(),
                "modules = [\"m\"]\n".to_string(),
            ),
        ];
        v.extend(
            extra
                .iter()
                .map(|(a, b)| ((*a).to_string(), (*b).to_string())),
        );
        v
    }

    fn load_build(files: &[(String, String)]) -> (tempfile::TempDir, Repo) {
        let refs: Vec<(&str, &str)> = files
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let dir = build(&refs);
        let repo = load(dir.path()).unwrap();
        (dir, repo)
    }

    #[test]
    fn filename_parse() {
        let cases = [
            (
                "config.fish@macos.append",
                "config.fish",
                Some("macos"),
                true,
            ),
            (
                "monitors.lua@desktop",
                "monitors.lua",
                Some("desktop"),
                false,
            ),
            ("config", "config", None, false),
            ("notes.append", "notes", None, true),
            ("a@b@c", "a@b", Some("c"), false),
            ("user@host.conf", "user@host.conf", None, false),
        ];
        for (input, name, variant, append) in cases {
            let got = parse_file_name(input).unwrap();
            assert_eq!(
                (got.0.as_str(), got.1.as_deref(), got.2),
                (name, variant, append),
                "{input}"
            );
        }
        assert!(parse_file_name("@macos").is_err());
    }

    #[test]
    fn parse_errors_have_location() {
        let dir = build(&[("modules/m/module.toml", "sync = \"auto\"\n\nbefore = 1\n")]);
        let err = load(dir.path()).unwrap_err().to_string();
        assert!(err.contains("modules/m/module.toml:3:"), "{err}");
    }

    #[test]
    fn unknown_sync_value_is_an_error() {
        let dir = build(&[("modules/m/module.toml", "sync = \"sometimes\"\n")]);
        assert!(load(dir.path()).is_err());
    }

    #[test]
    fn symlink_in_module_is_error() {
        let dir = build(&[("modules/m/home/.bashrc", "x\n")]);
        std::os::unix::fs::symlink("/etc/hosts", dir.path().join("modules/m/home/.other")).unwrap();
        let err = load(dir.path()).unwrap_err().to_string();
        assert!(err.contains("symlinks are not supported"), "{err}");
    }

    #[test]
    fn unknown_host_lists_hosts() {
        let (_d, repo) = load_build(&one_module(&[("modules/m/home/f", "x\n")]));
        let err = select(&repo, "laptop", &Os::parse("linux:arch"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("desktop") && err.contains("macbook"), "{err}");
    }

    #[test]
    fn duplicate_module_in_host_is_error() {
        let dir = build(&[
            ("hosts/desktop.toml", "modules = [\"m\", \"m\"]\n"),
            ("modules/m/home/f", "x\n"),
        ]);
        let err = load(dir.path()).unwrap_err().to_string();
        assert!(err.contains("listed twice"), "{err}");
    }

    #[test]
    fn host_named_after_an_os_is_an_error() {
        let dir = build(&[("hosts/arch.toml", "modules = []\n")]);
        assert!(load(dir.path()).is_err());
    }

    #[test]
    fn module_with_nothing_in_it_is_an_error() {
        let dir = build(&[("modules/m/README", "x\n")]);
        assert!(load(dir.path()).is_err());
    }

    #[test]
    fn an_empty_module_directory_is_valid() {
        let dir = build(&[("hosts/desktop.toml", "modules = []\n")]);
        std::fs::create_dir_all(dir.path().join("modules/m")).unwrap();
        assert!(load(dir.path()).unwrap().modules.contains_key("m"));
    }

    #[test]
    fn when_os_excludes_module() {
        let (_d, repo) = load_build(&one_module(&[
            ("modules/m/module.toml", "when.os = [\"arch\"]\n"),
            ("modules/m/home/f", "x\n"),
        ]));
        let os = Os {
            family: Family::Linux,
            distro: Some("ubuntu".into()),
            like: Vec::new(),
        };
        let sel = select(&repo, "desktop", &os).unwrap();
        assert!(sel.modules.is_empty());
        assert!(desired(&repo, &sel, &os).unwrap().is_empty());
    }

    #[test]
    fn when_os_matches_id_like() {
        let (_d, repo) = load_build(&one_module(&[
            ("modules/m/module.toml", "when.os = [\"arch\"]\n"),
            ("modules/m/home/f", "x\n"),
        ]));
        let os = Os {
            family: Family::Linux,
            distro: Some("omarchy".into()),
            like: vec!["arch".into()],
        };
        assert_eq!(select(&repo, "desktop", &os).unwrap().modules.len(), 1);
    }

    #[test]
    fn variant_precedence() {
        let (_d, repo) = load_build(&one_module(&[
            ("modules/m/home/f.lua", "base value\n"),
            ("modules/m/home/f.lua@linux", "linux value\n"),
            ("modules/m/home/f.lua@arch", "arch value\n"),
            ("modules/m/home/f.lua@desktop", "desktop value\n"),
            ("modules/m/home/f.lua@macbook", "macbook value\n"),
        ]));
        let os = Os::parse("linux:arch");
        let sel = select(&repo, "desktop", &os).unwrap();
        let ls = layers(&repo, &sel, &os).unwrap();
        let target: Target = "~/f.lua".parse().unwrap();
        let variants: Vec<Option<&str>> = ls[&target]
            .iter()
            .map(|l| match &l.source {
                crate::compose::LayerSource::Module { variant, .. } => variant.as_deref(),
                crate::compose::LayerSource::OsDefault(_) => None,
            })
            .collect();
        assert_eq!(
            variants,
            vec![None, Some("linux"), Some("arch"), Some("desktop")]
        );
        let d = desired(&repo, &sel, &os).unwrap();
        assert_eq!(d[0].content, b"desktop value\n");
    }

    #[test]
    fn variant_for_other_host_dropped() {
        let (_d, repo) = load_build(&one_module(&[
            ("modules/m/home/f.lua", "base value\n"),
            ("modules/m/home/f.lua@macbook", "macbook value\n"),
        ]));
        let os = Os::parse("linux:arch");
        let sel = select(&repo, "desktop", &os).unwrap();
        let ls = layers(&repo, &sel, &os).unwrap();
        assert_eq!(ls[&"~/f.lua".parse::<Target>().unwrap()].len(), 1);
    }

    #[test]
    fn defaults_only_target_not_managed() {
        let mut files = one_module(&[("modules/m/home/f.lua", "base value\n")]);
        files.push(("rig.toml".to_string(), "defaults = [\"d\"]\n".to_string()));
        files.push(("d/home/only.conf".to_string(), "only value\n".to_string()));
        let (_d, repo) = load_build(&files);
        let os = Os::parse("linux:arch");
        let sel = select(&repo, "desktop", &os).unwrap();
        let targets: Vec<String> = desired(&repo, &sel, &os)
            .unwrap()
            .iter()
            .map(|d| d.target.to_string())
            .collect();
        assert_eq!(targets, vec!["~/f.lua"]);
    }
}
