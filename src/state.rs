//! The manifest, the blob store and the conflict directory.

use crate::repo::{Backend, Target};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hash([u8; 32]);

impl Hash {
    #[must_use]
    pub fn of(content: &[u8]) -> Hash {
        Hash(*blake3::hash(content).as_bytes())
    }

    #[must_use]
    pub fn short(&self) -> String {
        self.to_string()[..8].to_string()
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for Hash {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Hash> {
        if s.len() != 64 {
            bail!("not a blake3 hash: {s}");
        }
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16)
                .with_context(|| format!("not a blake3 hash: {s}"))?;
        }
        Ok(Hash(out))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub hash: Hash,
    pub module: String,
    pub mode: u32,
}

/// How a package came to be in the ledger, which decides what leaving the repo means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// rig installed it, so rig removes it.
    Installed,
    /// It was already there when a module first listed it, so rig leaves it.
    Adopted,
}

pub type Tracked = BTreeMap<String, Origin>;

/// What a setting was before rig first wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Original {
    Value(crate::settings::Observed),
    Unset,
    /// Recorded before rig kept originals; nothing can be restored.
    Unknown,
}

impl Original {
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Original::Value(v) => v.value.clone(),
            Original::Unset => "unset".to_string(),
            Original::Unknown => "unknown".to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    /// What rig last wrote or adopted.
    pub current: String,
    pub original: Original,
    pub domain: Option<String>,
    pub key: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct State {
    pub entries: BTreeMap<Target, Entry>,
    pub packages: BTreeMap<Backend, Tracked>,
    pub macos_casks: BTreeSet<String>,
    /// `provider:domain.key` to what rig wrote and what it found.
    pub settings: BTreeMap<String, Setting>,
    /// `module: command` for every hook whose last run failed, so it runs again.
    pub failed_hooks: BTreeSet<String>,
}

/// Reads content that `reconcile` needs as the diff3 ancestor.
pub trait BlobSource {
    /// # Errors
    /// When the blob exists but cannot be read.
    fn blob(&self, hash: &Hash) -> Result<Option<Vec<u8>>>;
}

#[derive(Serialize, Deserialize)]
struct OriginalToml {
    value: String,
    kind: String,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum SettingToml {
    Full {
        current: String,
        /// Absent means unknown; `false` means the key was unset.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        original: Option<OriginalToml>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unset: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        domain: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    Legacy(String),
}

impl From<SettingToml> for Setting {
    fn from(t: SettingToml) -> Setting {
        match t {
            SettingToml::Legacy(current) => Setting {
                current,
                original: Original::Unknown,
                domain: None,
                key: None,
            },
            SettingToml::Full {
                current,
                original,
                unset,
                domain,
                key,
            } => Setting {
                current,
                original: match original {
                    Some(o) => Original::Value(crate::settings::Observed {
                        value: o.value,
                        kind: o.kind,
                    }),
                    None if unset => Original::Unset,
                    None => Original::Unknown,
                },
                domain,
                key,
            },
        }
    }
}

impl From<&Setting> for SettingToml {
    fn from(s: &Setting) -> SettingToml {
        SettingToml::Full {
            current: s.current.clone(),
            original: match &s.original {
                Original::Value(v) => Some(OriginalToml {
                    value: v.value.clone(),
                    kind: v.kind.clone(),
                }),
                _ => None,
            },
            unset: s.original == Original::Unset,
            domain: s.domain.clone(),
            key: s.key.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct EntryToml {
    hash: String,
    module: String,
    mode: String,
}

#[derive(Serialize, Deserialize, Default)]
struct MetaToml {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    macos_casks: BTreeSet<String>,
}

/// A backend's ledger; the list form predates `Origin` and means "rig installed these".
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum TrackedToml {
    Origins(Tracked),
    Names(BTreeSet<String>),
}

impl From<TrackedToml> for Tracked {
    fn from(t: TrackedToml) -> Tracked {
        match t {
            TrackedToml::Origins(m) => m,
            TrackedToml::Names(names) => {
                names.into_iter().map(|n| (n, Origin::Installed)).collect()
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    #[serde(default)]
    targets: BTreeMap<String, EntryToml>,
    #[serde(default)]
    packages: BTreeMap<Backend, TrackedToml>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    packages_meta: Option<MetaToml>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    settings: BTreeMap<String, SettingToml>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    failed_hooks: BTreeSet<String>,
}

pub struct Store {
    root: PathBuf,
    /// Blobs this run wants to survive one more `save`, so the user can fetch them.
    pinned: std::cell::RefCell<BTreeSet<Hash>>,
}

pub(crate) fn write_atomic(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let tmp = dir.join(format!(".{name}.rig-tmp-{}", std::process::id()));
    let result = (|| {
        std::fs::write(&tmp, content)?;
        std::fs::set_permissions(&tmp, PermissionsExt::from_mode(mode))?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result.with_context(|| format!("writing {}", path.display()))
}

impl Store {
    /// Opens a state directory, returning an empty state when it is missing or unreadable.
    ///
    /// # Errors
    /// When the manifest exists but is malformed or a future version.
    pub fn open(root: &Path) -> Result<(Store, State)> {
        let store = Store {
            root: root.to_path_buf(),
            pinned: std::cell::RefCell::default(),
        };
        let manifest = root.join("manifest.toml");
        let text = match std::fs::read_to_string(&manifest) {
            Ok(t) => t,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                return Ok((store, State::default()))
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", manifest.display())),
        };
        let m: Manifest =
            toml::from_str(&text).with_context(|| format!("parsing {}", manifest.display()))?;
        if m.version != 1 {
            bail!(
                "{}: unknown state version {}; delete {} and run rig up again",
                manifest.display(),
                m.version,
                root.display()
            );
        }
        let mut entries = BTreeMap::new();
        for (k, v) in m.targets {
            let target: Target = k
                .parse()
                .with_context(|| format!("{}: bad target key", manifest.display()))?;
            entries.insert(
                target,
                Entry {
                    hash: v.hash.parse()?,
                    module: v.module,
                    mode: u32::from_str_radix(&v.mode, 8)
                        .with_context(|| format!("{}: bad mode {}", manifest.display(), v.mode))?,
                },
            );
        }
        let state = State {
            entries,
            packages: m.packages.into_iter().map(|(b, t)| (b, t.into())).collect(),
            macos_casks: m.packages_meta.unwrap_or_default().macos_casks,
            settings: m.settings.into_iter().map(|(k, s)| (k, s.into())).collect(),
            failed_hooks: m.failed_hooks,
        };
        Ok((store, state))
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Writes the manifest atomically, then drops blobs no entry references.
    ///
    /// # Errors
    /// When the manifest cannot be written.
    pub fn save(&self, state: &State) -> Result<()> {
        let m = Manifest {
            version: 1,
            targets: state
                .entries
                .iter()
                .map(|(t, e)| {
                    (
                        t.to_string(),
                        EntryToml {
                            hash: e.hash.to_string(),
                            module: e.module.clone(),
                            mode: format!("{:04o}", e.mode),
                        },
                    )
                })
                .collect(),
            packages: state
                .packages
                .iter()
                .map(|(b, t)| (*b, TrackedToml::Origins(t.clone())))
                .collect(),
            settings: state
                .settings
                .iter()
                .map(|(k, s)| (k.clone(), s.into()))
                .collect(),
            failed_hooks: state.failed_hooks.clone(),
            packages_meta: (!state.macos_casks.is_empty()).then(|| MetaToml {
                macos_casks: state.macos_casks.clone(),
            }),
        };
        let text = toml::to_string(&m)?;
        write_atomic(&self.root.join("manifest.toml"), text.as_bytes(), 0o644)?;
        self.gc(state);
        Ok(())
    }

    /// Keeps a blob alive through this run's `save`; the next run collects it.
    pub fn pin(&self, hash: Hash) {
        self.pinned.borrow_mut().insert(hash);
    }

    fn gc(&self, state: &State) {
        let mut live: BTreeSet<String> =
            state.entries.values().map(|e| e.hash.to_string()).collect();
        live.extend(self.pinned.borrow().iter().map(ToString::to_string));
        let Ok(dir) = std::fs::read_dir(self.root.join("blobs")) else {
            return;
        };
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !live.contains(&name) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    /// Stores content and returns its hash.
    ///
    /// # Errors
    /// When the blob cannot be written.
    pub fn put_blob(&self, content: &[u8]) -> Result<Hash> {
        let h = Hash::of(content);
        write_atomic(&self.root.join("blobs").join(h.to_string()), content, 0o644)?;
        Ok(h)
    }

    fn conflict_path(&self, target: &Target) -> PathBuf {
        self.root.join("conflicts").join(target.module_path())
    }

    /// Writes conflict-marked content for a target and returns where it landed.
    ///
    /// # Errors
    /// When the file cannot be written.
    pub fn write_conflict(&self, target: &Target, marked: &[u8]) -> Result<PathBuf> {
        let p = self.conflict_path(target);
        write_atomic(&p, marked, 0o644)?;
        Ok(p)
    }

    /// Removes a target's conflict file if there is one.
    ///
    /// # Errors
    /// When the file exists but cannot be removed.
    pub fn clear_conflict(&self, target: &Target) -> Result<()> {
        let p = self.conflict_path(target);
        match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("removing {}", p.display()))
            }
            _ => Ok(()),
        }
    }

    /// Every target with an unresolved conflict file.
    ///
    /// # Errors
    /// When the conflicts directory cannot be walked.
    ///
    /// # Panics
    /// Never; every walked path sits under the conflicts root.
    pub fn conflicts(&self) -> Result<Vec<Target>> {
        let root = self.root.join("conflicts");
        let mut out = Vec::new();
        for e in walkdir::WalkDir::new(&root).sort_by_file_name() {
            let Ok(e) = e else { continue };
            if !e.file_type().is_file() {
                continue;
            }
            let rel = e.path().strip_prefix(&root).unwrap();
            if let Ok(t) = Target::from_module_path(rel) {
                out.push(t);
            }
        }
        Ok(out)
    }
}

impl BlobSource for Store {
    fn blob(&self, hash: &Hash) -> Result<Option<Vec<u8>>> {
        match std::fs::read(self.root.join("blobs").join(hash.to_string())) {
            Ok(c) => Ok(Some(c)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).context("reading blob"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_round_trips_through_hex() {
        let h = Hash::of(b"hello");
        assert_eq!(h.to_string().parse::<Hash>().unwrap(), h);
        assert_eq!(h.short().len(), 8);
    }

    #[test]
    fn manifest_round_trips() {
        let dir = tempfile::TempDir::new().unwrap();
        let (store, mut state) = Store::open(dir.path()).unwrap();
        let hash = store.put_blob(b"content\n").unwrap();
        state.entries.insert(
            "~/.config/ghostty/config".parse().unwrap(),
            Entry {
                hash,
                module: "ghostty".into(),
                mode: 0o644,
            },
        );
        state.packages.insert(
            Backend::Arch,
            [
                ("ghostty".to_string(), Origin::Installed),
                ("vim".to_string(), Origin::Adopted),
            ]
            .into(),
        );
        store.save(&state).unwrap();

        let (store2, back) = Store::open(dir.path()).unwrap();
        assert_eq!(back.entries, state.entries);
        assert_eq!(back.packages, state.packages);
        assert_eq!(store2.blob(&hash).unwrap().unwrap(), b"content\n");
    }

    #[test]
    fn settings_round_trip_and_legacy_strings_read_as_unknown() {
        let dir = tempfile::TempDir::new().unwrap();
        let (store, mut state) = Store::open(dir.path()).unwrap();
        let value = Setting {
            current: "1".into(),
            original: Original::Value(crate::settings::Observed {
                value: "2".into(),
                kind: "integer".into(),
            }),
            domain: Some("NSGlobalDomain".into()),
            key: Some("KeyRepeat".into()),
        };
        let unset = Setting {
            original: Original::Unset,
            ..value.clone()
        };
        state.settings.insert("macos:a.b".into(), value);
        state.settings.insert("macos:a.c".into(), unset);
        store.save(&state).unwrap();
        let (_, back) = Store::open(dir.path()).unwrap();
        assert_eq!(back.settings, state.settings);

        std::fs::write(
            dir.path().join("manifest.toml"),
            "version = 1\n[settings]\n\"macos:a.b\" = \"1\"\n",
        )
        .unwrap();
        let (_, legacy) = Store::open(dir.path()).unwrap();
        assert_eq!(legacy.settings["macos:a.b"].original, Original::Unknown);
        assert_eq!(legacy.settings["macos:a.b"].current, "1");
    }

    #[test]
    fn legacy_package_lists_read_as_installed() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("manifest.toml"),
            "version = 1\n[packages]\narch = [\"ghostty\"]\n",
        )
        .unwrap();
        let (_, state) = Store::open(dir.path()).unwrap();
        assert_eq!(
            state.packages[&Backend::Arch],
            [("ghostty".to_string(), Origin::Installed)].into()
        );
    }

    #[test]
    fn save_collects_unreferenced_blobs() {
        let dir = tempfile::TempDir::new().unwrap();
        let (store, state) = Store::open(dir.path()).unwrap();
        let h = store.put_blob(b"orphan\n").unwrap();
        store.save(&state).unwrap();
        assert!(store.blob(&h).unwrap().is_none());
    }

    #[test]
    fn a_pinned_blob_survives_this_run_and_dies_on_the_next() {
        let dir = tempfile::TempDir::new().unwrap();
        let (store, state) = Store::open(dir.path()).unwrap();
        let h = store.put_blob(b"previous\n").unwrap();
        store.pin(h);
        store.save(&state).unwrap();
        assert_eq!(store.blob(&h).unwrap().unwrap(), b"previous\n");

        let (next, state) = Store::open(dir.path()).unwrap();
        next.save(&state).unwrap();
        assert!(next.blob(&h).unwrap().is_none());
    }

    #[test]
    fn conflicts_round_trip() {
        let dir = tempfile::TempDir::new().unwrap();
        let (store, _) = Store::open(dir.path()).unwrap();
        let t: Target = "~/.config/hypr/bindings.lua".parse().unwrap();
        store.write_conflict(&t, b"<<<<<<<\n").unwrap();
        assert_eq!(store.conflicts().unwrap(), vec![t.clone()]);
        store.clear_conflict(&t).unwrap();
        assert!(store.conflicts().unwrap().is_empty());
    }

    #[test]
    fn unknown_version_is_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("manifest.toml"), "version = 2\n").unwrap();
        assert!(Store::open(dir.path()).is_err());
    }
}
