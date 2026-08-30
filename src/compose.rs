//! Layer ordering, format detection and the pure composition fold.

use crate::ini;
use crate::repo::{Format, ModuleFile, Os, Repo, Selection, Target};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayerSource {
    OsDefault(PathBuf),
    Module {
        name: String,
        variant: Option<String>,
        file: PathBuf,
    },
}

impl LayerSource {
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            LayerSource::OsDefault(p) | LayerSource::Module { file: p, .. } => p,
        }
    }

    #[must_use]
    pub fn module(&self) -> Option<&str> {
        match self {
            LayerSource::OsDefault(_) => None,
            LayerSource::Module { name, .. } => Some(name),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub source: LayerSource,
    pub content: Vec<u8>,
    pub append: bool,
    pub mode: u32,
}

#[derive(Clone, Debug)]
pub struct Desired {
    pub target: Target,
    pub content: Vec<u8>,
    pub mode: u32,
    pub module: String,
    pub format: Format,
    pub layers: Vec<LayerSource>,
}

/// Where a variant tag sorts within one source. `None` means the file is not for this host.
fn variant_rank(tag: Option<&str>, os: &Os, host: &str) -> Option<u8> {
    match tag {
        None => Some(0),
        Some(t) if t == "linux" || t == "macos" => os.matches(t).then_some(1),
        Some(t) if os.distro.as_deref() == Some(t) => Some(2),
        Some(t) if os.like.iter().any(|l| l == t) => Some(3),
        Some(t) if t == host => Some(4),
        Some(_) => None,
    }
}

fn read_layer(f: &ModuleFile, source: LayerSource) -> Result<Layer> {
    Ok(Layer {
        content: std::fs::read(&f.path).with_context(|| format!("reading {}", f.path.display()))?,
        source,
        append: f.append,
        mode: f.mode,
    })
}

fn push_source<'a>(
    out: &mut BTreeMap<Target, Vec<Layer>>,
    files: impl Iterator<Item = &'a ModuleFile>,
    os: &Os,
    host: &str,
    make: impl Fn(&ModuleFile) -> LayerSource,
) -> Result<()> {
    let mut ranked: Vec<(u8, &Path, &ModuleFile)> = files
        .filter_map(|f| {
            variant_rank(f.variant.as_deref(), os, host).map(|r| (r, f.path.as_path(), f))
        })
        .collect();
    ranked.sort_by_key(|(r, p, _)| (*r, *p));
    for (_, _, f) in ranked {
        out.entry(f.target.clone())
            .or_default()
            .push(read_layer(f, make(f))?);
    }
    Ok(())
}

/// Reads every contributing file and orders the layers per target, bottom to top.
///
/// # Errors
/// When a repo file cannot be read.
pub fn layers(repo: &Repo, sel: &Selection, os: &Os) -> Result<BTreeMap<Target, Vec<Layer>>> {
    let mut out: BTreeMap<Target, Vec<Layer>> = BTreeMap::new();
    for d in &repo.defaults {
        push_source(&mut out, d.files.iter(), os, &sel.host, |f| {
            LayerSource::OsDefault(f.path.clone())
        })?;
    }
    for m in &sel.modules {
        push_source(&mut out, m.files.iter(), os, &sel.host, |f| {
            LayerSource::Module {
                name: m.name.clone(),
                variant: f.variant.clone(),
                file: f.path.clone(),
            }
        })?;
    }
    Ok(out)
}

fn is_json(b: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(b).is_ok()
}

fn is_toml(b: &[u8]) -> bool {
    std::str::from_utf8(b).is_ok_and(|s| s.parse::<toml_edit::DocumentMut>().is_ok())
}

fn is_ini(b: &[u8]) -> bool {
    std::str::from_utf8(b).is_ok_and(|s| ini::parse(s).is_ok())
}

/// Extension first, then sniffing every non-append, non-empty layer.
#[must_use]
pub fn detect_format(target: &Target, layers: &[Layer]) -> Format {
    let rel = target.rel();
    let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
    match rel.extension().and_then(|e| e.to_str()) {
        Some("toml") => return Format::Toml,
        Some("json") => return Format::Json,
        Some("ini" | "desktop" | "gitconfig") => return Format::Ini,
        _ => {}
    }
    if name == ".gitconfig" || rel == Path::new(".config/git/config") {
        return Format::Ini;
    }
    let bodies: Vec<&[u8]> = layers
        .iter()
        .filter(|l| !l.append && !l.content.is_empty())
        .map(|l| l.content.as_slice())
        .collect();
    if bodies.is_empty() {
        Format::Text
    } else if bodies.iter().all(|b| is_json(b)) {
        Format::Json
    } else if bodies.iter().all(|b| is_toml(b)) {
        Format::Toml
    } else if bodies.iter().all(|b| is_ini(b)) {
        Format::Ini
    } else {
        Format::Text
    }
}

fn merge_inline(into: &mut toml_edit::InlineTable, from: &toml_edit::InlineTable) {
    for (k, v) in from {
        let nested = matches!(
            (into.get_mut(k), v),
            (
                Some(toml_edit::Value::InlineTable(_)),
                toml_edit::Value::InlineTable(_)
            )
        );
        if nested {
            let (Some(toml_edit::Value::InlineTable(i)), toml_edit::Value::InlineTable(f)) =
                (into.get_mut(k), v)
            else {
                unreachable!()
            };
            merge_inline(i, f);
        } else {
            into.insert_formatted(&from.key(k).unwrap().clone(), v.clone());
        }
    }
}

fn merge_tables(into: &mut toml_edit::Table, from: &toml_edit::Table) {
    use toml_edit::{Item, Value};
    for (k, item) in from {
        let nested = matches!(
            (into.get(k), item),
            (Some(Item::Table(_)), Item::Table(_))
                | (
                    Some(Item::Value(Value::InlineTable(_))),
                    Item::Value(Value::InlineTable(_))
                )
        );
        if nested {
            match (into.get_mut(k).unwrap(), item) {
                (Item::Table(i), Item::Table(f)) => merge_tables(i, f),
                (Item::Value(Value::InlineTable(i)), Item::Value(Value::InlineTable(f))) => {
                    merge_inline(i, f);
                }
                _ => unreachable!(),
            }
        } else {
            into.insert_formatted(&from.key(k).unwrap().clone(), item.clone());
        }
    }
}

fn parse_doc(bytes: &[u8], what: &Path) -> Result<toml_edit::DocumentMut> {
    let text =
        std::str::from_utf8(bytes).with_context(|| format!("{}: not utf-8", what.display()))?;
    text.parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("{}: invalid toml", what.display()))
}

fn merge_json(into: &mut serde_json::Value, from: &serde_json::Value) {
    match (into, from) {
        (serde_json::Value::Object(i), serde_json::Value::Object(f)) => {
            for (k, v) in f {
                match i.get_mut(k) {
                    Some(iv) if iv.is_object() && v.is_object() => merge_json(iv, v),
                    _ => {
                        i.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (i, f) => *i = f.clone(),
    }
}

/// Serializes JSON the way rig writes it: two space indent, trailing newline.
///
/// # Errors
/// When the value cannot be serialized.
pub fn json_to_bytes(v: &serde_json::Value) -> Result<Vec<u8>> {
    let mut s = serde_json::to_string_pretty(v)?;
    s.push('\n');
    Ok(s.into_bytes())
}

fn append(acc: &[u8], add: &[u8]) -> Vec<u8> {
    let mut out = acc.to_vec();
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.extend_from_slice(add);
    out
}

/// Folds layers bottom to top into the exact bytes rig wants on disk.
///
/// # Errors
/// When a layer does not parse as the detected format.
///
/// # Panics
/// Never; the empty case is rejected above.
pub fn compose(target: &Target, layers: &[Layer]) -> Result<Desired> {
    if layers.is_empty() {
        bail!("{target}: no layers");
    }
    let format = detect_format(target, layers);
    let mut acc = layers[0].content.clone();
    for l in &layers[1..] {
        if l.append {
            acc = append(&acc, &l.content);
            continue;
        }
        let path = l.source.path();
        acc = match format {
            Format::Text => l.content.clone(),
            Format::Toml => {
                let mut base = parse_doc(&acc, layers[0].source.path())?;
                let upper = parse_doc(&l.content, path)?;
                merge_tables(base.as_table_mut(), upper.as_table());
                base.to_string().into_bytes()
            }
            Format::Json => {
                let mut base: serde_json::Value =
                    serde_json::from_slice(&acc).with_context(|| {
                        format!("{}: invalid json", layers[0].source.path().display())
                    })?;
                let upper: serde_json::Value = serde_json::from_slice(&l.content)
                    .with_context(|| format!("{}: invalid json", path.display()))?;
                merge_json(&mut base, &upper);
                json_to_bytes(&base)?
            }
            Format::Ini => {
                let bottom = layers[0].source.path();
                let base_text = std::str::from_utf8(&acc)
                    .with_context(|| format!("{}: not utf-8", bottom.display()))?;
                let mut base = ini::parse(base_text)
                    .map_err(|line| anyhow::anyhow!("{}:{line}: invalid ini", bottom.display()))?;
                let upper_text = std::str::from_utf8(&l.content)
                    .with_context(|| format!("{}: not utf-8", path.display()))?;
                let upper = ini::parse(upper_text)
                    .map_err(|line| anyhow::anyhow!("{}:{line}: invalid ini", path.display()))?;
                ini::merge(&mut base, &upper);
                base.render().into_bytes()
            }
        };
    }
    let top = layers.last().unwrap();
    let module = layers
        .iter()
        .rev()
        .find_map(|l| l.source.module())
        .unwrap_or_default()
        .to_string();
    Ok(Desired {
        target: target.clone(),
        content: acc,
        mode: top.mode,
        module,
        format,
        layers: layers.iter().map(|l| l.source.clone()).collect(),
    })
}

/// Everything rig wants on disk for this host. Targets only OS defaults contribute to are dropped.
///
/// # Errors
/// When a layer cannot be read or composed.
///
/// # Panics
/// Never; the inner `compose` always sees a non-empty layer list.
pub fn desired(repo: &Repo, sel: &Selection, os: &Os) -> Result<Vec<Desired>> {
    let mut out = Vec::new();
    for (target, ls) in layers(repo, sel, os)? {
        if !ls.iter().any(|l| l.source.module().is_some()) {
            continue;
        }
        out.push(compose(&target, &ls)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::Format;

    fn layer(name: &str, content: &str) -> Layer {
        Layer {
            source: LayerSource::Module {
                name: name.to_string(),
                variant: None,
                file: PathBuf::from(name),
            },
            content: content.as_bytes().to_vec(),
            append: false,
            mode: 0o644,
        }
    }

    fn fold(target: &str, layers: &[Layer]) -> String {
        let t: Target = target.parse().unwrap();
        String::from_utf8(compose(&t, layers).unwrap().content).unwrap()
    }

    #[test]
    fn toml_deep_merge() {
        let out = fold(
            "~/x.toml",
            &[
                layer("lo", "[a]\n# keep me\nx = 1\ny = 2\n"),
                layer("hi", "[a]\ny = 3\n\n[b]\nz = 1\n"),
            ],
        );
        assert!(out.contains("# keep me"), "{out}");
        assert!(out.contains("x = 1"), "{out}");
        assert!(out.contains("y = 3") && !out.contains("y = 2"), "{out}");
        assert!(out.contains("[b]") && out.contains("z = 1"), "{out}");
    }

    #[test]
    fn toml_array_replaces() {
        let out = fold(
            "~/x.toml",
            &[
                layer("lo", "list = [1, 2, 3]\n\n[[server]]\nname = \"a\"\n"),
                layer("hi", "list = [4]\n\n[[server]]\nname = \"b\"\n"),
            ],
        );
        assert!(out.contains("list = [4]"), "{out}");
        assert!(
            out.contains("name = \"b\"") && !out.contains("name = \"a\""),
            "{out}"
        );
    }

    #[test]
    fn toml_format_of_upper_wins() {
        let out = fold(
            "~/x.toml",
            &[
                layer("lo", "a = 1\n"),
                layer("hi", "# upper comment\nb = 2\n"),
            ],
        );
        assert!(out.contains("# upper comment\nb = 2"), "{out}");
    }

    #[test]
    fn json_deep_merge() {
        let out = fold(
            "~/x.json",
            &[
                layer("lo", r#"{"z": 1, "a": {"p": 1, "q": 2}, "l": [1, 2]}"#),
                layer("hi", r#"{"a": {"q": 3}, "l": [9]}"#),
            ],
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["a"]["p"], 1);
        assert_eq!(v["a"]["q"], 3);
        assert_eq!(v["l"], serde_json::json!([9]));
        assert!(out.starts_with("{\n  \"z\": 1,"), "{out}");
        assert!(out.ends_with("}\n"), "{out}");
    }

    #[test]
    fn append_adds_newline_once() {
        let mut top = layer("hi", "tail\n");
        top.append = true;
        let with = fold("~/x.fish", &[layer("lo", "head\n"), top.clone()]);
        let without = fold("~/x.fish", &[layer("lo", "head"), top]);
        assert_eq!(with, "head\ntail\n");
        assert_eq!(without, "head\ntail\n");
    }

    #[test]
    fn text_last_wins() {
        assert_eq!(
            fold(
                "~/x.lua",
                &[layer("lo", "one two\n"), layer("hi", "three four\n")]
            ),
            "three four\n"
        );
    }

    #[test]
    fn detect_format_sniffs_ini_not_toml() {
        let ghostty = layer(
            "g",
            "theme = JetBrains Darcula\nfont-size = 20\nkeybind = a=b\n",
        );
        let t: Target = "~/.config/ghostty/config".parse().unwrap();
        assert_eq!(
            detect_format(&t, std::slice::from_ref(&ghostty)),
            Format::Ini
        );
    }

    #[test]
    fn detect_format_text_for_lua() {
        let lua = layer(
            "l",
            "return {\n  { \"SUPER, Return\", \"exec, ghostty\" },\n}\n",
        );
        let t: Target = "~/.config/hypr/bindings.lua".parse().unwrap();
        assert_eq!(detect_format(&t, std::slice::from_ref(&lua)), Format::Text);
    }

    #[test]
    fn detect_format_by_target_name() {
        let l = layer("g", "[user]\n\tname = A\n");
        let ls = std::slice::from_ref(&l);
        assert_eq!(
            detect_format(&"~/.config/git/config".parse().unwrap(), ls),
            Format::Ini
        );
        assert_eq!(
            detect_format(&"~/.gitconfig".parse().unwrap(), ls),
            Format::Ini
        );
    }
}
