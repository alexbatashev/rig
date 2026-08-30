//! Carrying an edit made on disk back into the repo layer that owns it.

use crate::ini;
use crate::repo::Format;
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Key {
        path: String,
        from: Option<String>,
        to: Option<String>,
    },
    Lines {
        added: usize,
        removed: usize,
    },
}

impl Change {
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Change::Key { path, from, to } => match (from, to) {
                (Some(f), Some(t)) => format!("{path}: {f} -> {t}"),
                (None, Some(t)) => format!("{path}: added {t}"),
                (Some(_), None) => format!("{path}: removed"),
                (None, None) => path.clone(),
            },
            Change::Lines { added, removed } => format!("+{added} -{removed} lines"),
        }
    }
}

/// One line summary of what absorb carried over.
#[must_use]
pub fn summary(changes: &[Change]) -> String {
    if changes.len() > 4 {
        return format!("{} changes", changes.len());
    }
    changes
        .iter()
        .map(Change::render)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The layer below the one being patched, for `.append` and for refusal messages.
pub struct Lower {
    pub file: PathBuf,
    pub content: Vec<u8>,
}

/// The top-most module layer for a target, which is where an edit lands.
pub struct TopLayer {
    pub file: PathBuf,
    pub content: Vec<u8>,
    pub append: bool,
    pub lower: Option<Lower>,
}

#[derive(Debug)]
pub struct AbsorbEdit {
    pub layer_file: PathBuf,
    pub new_content: Vec<u8>,
    pub changes: Vec<Change>,
}

/// Rewrites the top layer so that composing again yields the content now on disk.
///
/// `base` is what rig last wrote, `disk` is what is there now. Pure over bytes.
///
/// # Errors
/// When the edit cannot be expressed as a change to this layer.
pub fn absorb(format: Format, layer: &TopLayer, base: &[u8], disk: &[u8]) -> Result<AbsorbEdit> {
    if layer.append && format != Format::Text {
        bail!(
            "top layer is .append; edit {} directly",
            layer.file.display()
        );
    }
    let (new_content, changes) = match format {
        Format::Toml => keyed(layer, base, disk, toml_keys, toml_apply)?,
        Format::Json => keyed(layer, base, disk, json_keys, json_apply)?,
        Format::Ini => keyed(layer, base, disk, ini_keys, ini_apply)?,
        Format::Text => text(layer, base, disk)?,
    };
    Ok(AbsorbEdit {
        layer_file: layer.file.clone(),
        new_content,
        changes,
    })
}

type Keys = BTreeMap<String, String>;
type KeyReader = fn(&[u8], &str) -> Result<Keys>;
type KeyWriter = fn(&[u8], &[Change], &TopLayer) -> Result<Vec<u8>>;

fn diff_keys(base: &Keys, disk: &Keys) -> Vec<Change> {
    let mut out = Vec::new();
    for (k, v) in disk {
        if base.get(k) != Some(v) {
            out.push(Change::Key {
                path: k.clone(),
                from: base.get(k).cloned(),
                to: Some(v.clone()),
            });
        }
    }
    for k in base.keys() {
        if !disk.contains_key(k) {
            out.push(Change::Key {
                path: k.clone(),
                from: base.get(k).cloned(),
                to: None,
            });
        }
    }
    out
}

fn keyed(
    layer: &TopLayer,
    base: &[u8],
    disk: &[u8],
    keys: KeyReader,
    apply: KeyWriter,
) -> Result<(Vec<u8>, Vec<Change>)> {
    let base_keys = keys(base, "the recorded content")?;
    let disk_keys = keys(disk, "the file on disk")?;
    let changes = diff_keys(&base_keys, &disk_keys);
    if changes.is_empty() {
        return Ok((layer.content.clone(), changes));
    }
    let content = apply(&layer.content, &changes, layer)?;
    Ok((content, changes))
}

fn lower_label(layer: &TopLayer) -> String {
    layer.lower.as_ref().map_or_else(
        || "a lower layer".to_string(),
        |l| l.file.display().to_string(),
    )
}

fn refuse_removal(path: &str, layer: &TopLayer) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot absorb removal of {path}: it comes from {}; edit that file",
        lower_label(layer)
    )
}

// TOML.

fn toml_doc(bytes: &[u8], what: &str) -> Result<toml_edit::DocumentMut> {
    std::str::from_utf8(bytes)
        .context("not utf-8")
        .and_then(|s| Ok(s.parse::<toml_edit::DocumentMut>()?))
        .with_context(|| format!("{what} no longer parses as toml; fix it or rig up --force"))
}

fn toml_flatten(table: &toml_edit::Table, prefix: &str, out: &mut Keys) {
    for (k, item) in table {
        let path = if prefix.is_empty() {
            k.to_string()
        } else {
            format!("{prefix}.{k}")
        };
        match item {
            toml_edit::Item::Table(inner) => toml_flatten(inner, &path, out),
            other => {
                out.insert(path, other.to_string().trim().to_string());
            }
        }
    }
}

fn toml_keys(bytes: &[u8], what: &str) -> Result<Keys> {
    let mut out = Keys::new();
    toml_flatten(toml_doc(bytes, what)?.as_table(), "", &mut out);
    Ok(out)
}

fn toml_parent<'a>(
    doc: &'a mut toml_edit::DocumentMut,
    path: &str,
    create: bool,
) -> Option<(&'a mut toml_edit::Table, String)> {
    let parts: Vec<&str> = path.split('.').collect();
    let (last, parents) = parts.split_last()?;
    let mut table = doc.as_table_mut();
    for p in parents {
        if table.get(p).is_none() {
            if !create {
                return None;
            }
            table.insert(p, toml_edit::Item::Table(toml_edit::Table::new()));
        }
        table = table.get_mut(p)?.as_table_mut()?;
    }
    Some((table, (*last).to_string()))
}

fn toml_apply(layer: &[u8], changes: &[Change], top: &TopLayer) -> Result<Vec<u8>> {
    let mut doc = toml_doc(layer, "the layer file")?;
    for c in changes {
        let Change::Key { path, to, .. } = c else {
            continue;
        };
        if let Some(v) = to {
            let parsed: toml_edit::DocumentMut = format!("x = {v}\n")
                .parse()
                .with_context(|| format!("cannot absorb {path}: value {v} is not a scalar"))?;
            let item = parsed["x"].clone();
            let (table, key) = toml_parent(&mut doc, path, true)
                .with_context(|| format!("cannot absorb {path}"))?;
            // Assigning through the existing entry keeps the key's decor, so comments survive.
            match table.get_mut(&key) {
                Some(existing) => *existing = item,
                None => {
                    table.insert(&key, item);
                }
            }
        } else {
            let removed =
                toml_parent(&mut doc, path, false).is_some_and(|(t, k)| t.remove(&k).is_some());
            if !removed {
                return Err(refuse_removal(path, top));
            }
        }
    }
    Ok(doc.to_string().into_bytes())
}

// JSON.

fn json_value(bytes: &[u8], what: &str) -> Result<serde_json::Value> {
    serde_json::from_slice(bytes)
        .with_context(|| format!("{what} no longer parses as json; fix it or rig up --force"))
}

fn json_flatten(v: &serde_json::Value, prefix: &str, out: &mut Keys) {
    match v {
        serde_json::Value::Object(map) => {
            for (k, item) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                json_flatten(item, &path, out);
            }
        }
        other => {
            out.insert(prefix.to_string(), other.to_string());
        }
    }
}

fn json_keys(bytes: &[u8], what: &str) -> Result<Keys> {
    let mut out = Keys::new();
    json_flatten(&json_value(bytes, what)?, "", &mut out);
    Ok(out)
}

fn json_parent<'a>(
    v: &'a mut serde_json::Value,
    path: &str,
    create: bool,
) -> Option<(&'a mut serde_json::Map<String, serde_json::Value>, String)> {
    let parts: Vec<&str> = path.split('.').collect();
    let (last, parents) = parts.split_last()?;
    let mut node = v;
    for p in parents {
        let map = node.as_object_mut()?;
        if !map.contains_key(*p) {
            if !create {
                return None;
            }
            map.insert((*p).to_string(), serde_json::json!({}));
        }
        node = map.get_mut(*p)?;
    }
    Some((node.as_object_mut()?, (*last).to_string()))
}

fn json_apply(layer: &[u8], changes: &[Change], top: &TopLayer) -> Result<Vec<u8>> {
    let mut doc = json_value(layer, "the layer file")?;
    for c in changes {
        let Change::Key { path, to, .. } = c else {
            continue;
        };
        if let Some(v) = to {
            let value: serde_json::Value = serde_json::from_str(v)
                .with_context(|| format!("cannot absorb {path}: value {v} is not json"))?;
            let (map, key) = json_parent(&mut doc, path, true)
                .with_context(|| format!("cannot absorb {path}"))?;
            map.insert(key, value);
        } else {
            let removed =
                json_parent(&mut doc, path, false).is_some_and(|(m, k)| m.remove(&k).is_some());
            if !removed {
                return Err(refuse_removal(path, top));
            }
        }
    }
    crate::compose::json_to_bytes(&doc)
}

// INI.

fn ini_doc(bytes: &[u8], what: &str) -> Result<ini::Ini> {
    let text = std::str::from_utf8(bytes).with_context(|| format!("{what}: not utf-8"))?;
    ini::parse(text).map_err(|line| anyhow::anyhow!("{what} no longer parses as ini (line {line})"))
}

fn ini_path(section: Option<&str>, key: &str) -> String {
    section.map_or_else(|| key.to_string(), |s| format!("{s}.{key}"))
}

fn split_ini_path(path: &str) -> (Option<&str>, &str) {
    match path.rsplit_once('.') {
        Some((s, k)) => (Some(s), k),
        None => (None, path),
    }
}

fn ini_keys(bytes: &[u8], what: &str) -> Result<Keys> {
    let doc = ini_doc(bytes, what)?;
    let mut out: Keys = Keys::new();
    for (section, key, value) in doc.entries() {
        out.entry(ini_path(section, key))
            .and_modify(|v| {
                v.push('\n');
                v.push_str(value);
            })
            .or_insert_with(|| value.to_string());
    }
    Ok(out)
}

fn ini_apply(layer: &[u8], changes: &[Change], top: &TopLayer) -> Result<Vec<u8>> {
    let mut doc = ini_doc(layer, "the layer file")?;
    for c in changes {
        let Change::Key { path, to, .. } = c else {
            continue;
        };
        let (section, key) = split_ini_path(path);
        if let Some(v) = to {
            let values: Vec<String> = v.split('\n').map(ToString::to_string).collect();
            doc.set(section, key, &values);
        } else if !doc.remove(section, key) {
            return Err(refuse_removal(path, top));
        }
    }
    Ok(doc.render().into_bytes())
}

// Text.

fn append_separator(lower: &[u8]) -> Vec<u8> {
    let mut out = lower.to_vec();
    if !out.is_empty() && !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    out
}

fn line_counts(patch: &diffy::Patch<'_, [u8]>) -> Change {
    let mut added = 0;
    let mut removed = 0;
    for hunk in patch.hunks() {
        for line in hunk.lines() {
            match line {
                diffy::Line::Insert(_) => added += 1,
                diffy::Line::Delete(_) => removed += 1,
                diffy::Line::Context(_) => {}
            }
        }
    }
    Change::Lines { added, removed }
}

fn text(layer: &TopLayer, base: &[u8], disk: &[u8]) -> Result<(Vec<u8>, Vec<Change>)> {
    let patch = diffy::create_patch_bytes(base, disk);
    let changes = if patch.hunks().is_empty() {
        Vec::new()
    } else {
        vec![line_counts(&patch)]
    };
    if layer.append {
        let prefix = append_separator(
            &layer
                .lower
                .as_ref()
                .map_or_else(Vec::new, |l| l.content.clone()),
        );
        if !disk.starts_with(&prefix) {
            bail!(
                "edit does not apply to {} because lower layers changed the context; edit the module file directly or rig adopt",
                layer.file.display()
            );
        }
        return Ok((disk[prefix.len()..].to_vec(), changes));
    }
    if layer.content == base {
        return Ok((disk.to_vec(), changes));
    }
    let new = diffy::apply_bytes(&layer.content, &patch).map_err(|_| {
        anyhow::anyhow!(
            "edit does not apply to {} because lower layers changed the context; edit the module file directly or rig adopt",
            layer.file.display()
        )
    })?;
    Ok((new, changes))
}

/// Whether two byte strings mean the same thing in this format, ignoring formatting.
#[must_use]
pub fn equivalent(format: Format, a: &[u8], b: &[u8]) -> bool {
    match format {
        Format::Text => a == b,
        Format::Toml => {
            match (
                std::str::from_utf8(a)
                    .ok()
                    .and_then(|s| s.parse::<toml::Value>().ok()),
                std::str::from_utf8(b)
                    .ok()
                    .and_then(|s| s.parse::<toml::Value>().ok()),
            ) {
                (Some(x), Some(y)) => x == y,
                _ => a == b,
            }
        }
        Format::Json => match (json_value(a, ""), json_value(b, "")) {
            (Ok(x), Ok(y)) => x == y,
            _ => a == b,
        },
        Format::Ini => match (ini_doc(a, ""), ini_doc(b, "")) {
            (Ok(x), Ok(y)) => x.entries() == y.entries(),
            _ => a == b,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top(content: &str) -> TopLayer {
        TopLayer {
            file: PathBuf::from("modules/m/home/f"),
            content: content.as_bytes().to_vec(),
            append: false,
            lower: None,
        }
    }

    fn with_lower(content: &str, lower: &str) -> TopLayer {
        TopLayer {
            lower: Some(Lower {
                file: PathBuf::from("modules/m/home/f.base"),
                content: lower.as_bytes().to_vec(),
            }),
            ..top(content)
        }
    }

    fn run(format: Format, layer: &TopLayer, base: &str, disk: &str) -> AbsorbEdit {
        absorb(format, layer, base.as_bytes(), disk.as_bytes()).unwrap()
    }

    fn text_of(edit: &AbsorbEdit) -> String {
        String::from_utf8(edit.new_content.clone()).unwrap()
    }

    #[test]
    fn toml_single_key_change() {
        let layer = top("[character]\n# the prompt arrow\nsuccess_symbol = \"a\"\n");
        let edit = run(
            Format::Toml,
            &layer,
            "[character]\nsuccess_symbol = \"a\"\n",
            "[character]\nsuccess_symbol = \"b\"\n",
        );
        assert_eq!(
            text_of(&edit),
            "[character]\n# the prompt arrow\nsuccess_symbol = \"b\"\n"
        );
        assert_eq!(
            edit.changes,
            vec![Change::Key {
                path: "character.success_symbol".into(),
                from: Some("\"a\"".into()),
                to: Some("\"b\"".into()),
            }]
        );
    }

    #[test]
    fn toml_key_added_lands_in_layer() {
        let layer = top("[directory]\ntruncation_length = 5\n");
        let edit = run(
            Format::Toml,
            &layer,
            "[directory]\ntruncation_length = 5\n",
            "[directory]\ntruncation_length = 5\ntruncate_to_repo = false\n",
        );
        assert!(text_of(&edit).contains("truncate_to_repo = false"));
    }

    #[test]
    fn toml_key_removed_from_layer() {
        let layer = top("[a]\nx = 1\ny = 2\n");
        let edit = run(Format::Toml, &layer, "[a]\nx = 1\ny = 2\n", "[a]\nx = 1\n");
        assert_eq!(text_of(&edit), "[a]\nx = 1\n");
    }

    #[test]
    fn toml_removal_from_lower_layer_refused() {
        let layer = with_lower("[a]\nx = 1\n", "[a]\ny = 2\n");
        let err = absorb(
            Format::Toml,
            &layer,
            b"[a]\nx = 1\ny = 2\n",
            b"[a]\nx = 1\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("cannot absorb removal of a.y"), "{err}");
        assert!(err.contains("f.base"), "{err}");
    }

    #[test]
    fn toml_nested_key_path() {
        let layer = top("");
        let edit = run(Format::Toml, &layer, "", "[a.b]\nc = 1\n");
        assert_eq!(
            edit.changes[0],
            Change::Key {
                path: "a.b.c".into(),
                from: None,
                to: Some("1".into()),
            }
        );
        assert!(text_of(&edit).contains("c = 1"), "{}", text_of(&edit));
    }

    #[test]
    fn toml_array_change_is_one_key() {
        let layer = top("list = [1, 2]\n");
        let edit = run(Format::Toml, &layer, "list = [1, 2]\n", "list = [3]\n");
        assert_eq!(edit.changes.len(), 1);
        assert_eq!(text_of(&edit), "list = [3]\n");
    }

    #[test]
    fn json_key_change_pretty_output() {
        let layer = top("{\"a\": {\"b\": 1}}");
        let edit = run(
            Format::Json,
            &layer,
            "{\"a\": {\"b\": 1}}",
            "{\"a\": {\"b\": 2}}",
        );
        assert_eq!(edit.changes[0].render(), "a.b: 1 -> 2");
        assert_eq!(text_of(&edit), "{\n  \"a\": {\n    \"b\": 2\n  }\n}\n");
    }

    #[test]
    fn ini_value_change_keeps_tab() {
        let layer = top("[user]\n\tsigningkey = a\n");
        let edit = run(
            Format::Ini,
            &layer,
            "[user]\n\tsigningkey = a\n",
            "[user]\n\tsigningkey = b\n",
        );
        assert_eq!(text_of(&edit), "[user]\n\tsigningkey = b\n");
        assert_eq!(edit.changes[0].render(), "user.signingkey: a -> b");
    }

    #[test]
    fn ini_duplicate_key_group_change() {
        let layer = top("theme = dark\nkeybind = a=b\n");
        let edit = run(
            Format::Ini,
            &layer,
            "theme = dark\nkeybind = a=b\n",
            "theme = dark\nkeybind = c=d\nkeybind = e=f\n",
        );
        assert_eq!(edit.changes.len(), 1);
        assert_eq!(
            text_of(&edit),
            "theme = dark\nkeybind = c=d\nkeybind = e=f\n"
        );
    }

    #[test]
    fn ini_new_section_added() {
        let layer = top("[user]\n\tname = A\n");
        let edit = run(
            Format::Ini,
            &layer,
            "[user]\n\tname = A\n",
            "[user]\n\tname = A\n[core]\n\teditor = hx\n",
        );
        assert!(text_of(&edit).contains("[core]"), "{}", text_of(&edit));
        assert!(text_of(&edit).contains("editor = hx"));
    }

    #[test]
    fn ini_subsection_quoted_path() {
        let base = "[credential \"https://gist.github.com\"]\n\thelper = a\n";
        let disk = "[credential \"https://gist.github.com\"]\n\thelper = gh\n";
        let layer = top(base);
        let edit = run(Format::Ini, &layer, base, disk);
        assert_eq!(
            edit.changes[0].render(),
            "credential \"https://gist.github.com\".helper: a -> gh"
        );
        assert_eq!(text_of(&edit), disk);
    }

    #[test]
    fn text_single_layer_copies_disk() {
        let layer = top("one\ntwo\nthree\n");
        let edit = run(
            Format::Text,
            &layer,
            "one\ntwo\nthree\n",
            "one\nTWO\nthree\n",
        );
        assert_eq!(text_of(&edit), "one\nTWO\nthree\n");
        assert_eq!(
            edit.changes,
            vec![Change::Lines {
                added: 1,
                removed: 1
            }]
        );
    }

    #[test]
    fn text_patch_applies_to_upper_layer() {
        // The layer carries an extra trailing line the composed base does not have.
        let layer = top("one\ntwo\nthree\nfour\nfive\nsix\n");
        let edit = run(
            Format::Text,
            &layer,
            "one\ntwo\nthree\nfour\nfive\n",
            "ONE\ntwo\nthree\nfour\nfive\n",
        );
        assert_eq!(text_of(&edit), "ONE\ntwo\nthree\nfour\nfive\nsix\n");
    }

    #[test]
    fn text_patch_fails_on_context_mismatch() {
        let layer = top("totally\ndifferent\ncontent\n");
        let err = absorb(
            Format::Text,
            &layer,
            b"one\ntwo\nthree\nfour\nfive\n",
            b"ONE\ntwo\nthree\nfour\nfive\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("edit does not apply"), "{err}");
    }

    #[test]
    fn text_append_fragment_patched() {
        let layer = TopLayer {
            append: true,
            ..with_lower("fish_add_path /opt/homebrew/bin\n", "set -gx EDITOR hx\n")
        };
        let edit = run(
            Format::Text,
            &layer,
            "set -gx EDITOR hx\nfish_add_path /opt/homebrew/bin\n",
            "set -gx EDITOR hx\nfish_add_path /opt/homebrew/bin\nfish_add_path /extra\n",
        );
        assert_eq!(
            text_of(&edit),
            "fish_add_path /opt/homebrew/bin\nfish_add_path /extra\n"
        );
    }

    #[test]
    fn text_append_prefix_changed_refused() {
        let layer = TopLayer {
            append: true,
            ..with_lower("fish_add_path /opt/homebrew/bin\n", "set -gx EDITOR hx\n")
        };
        let err = absorb(
            Format::Text,
            &layer,
            b"set -gx EDITOR hx\nfish_add_path /opt/homebrew/bin\n",
            b"set -gx EDITOR vim\nfish_add_path /opt/homebrew/bin\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("edit does not apply"), "{err}");
    }

    #[test]
    fn append_layer_refused_for_structured_formats() {
        let layer = TopLayer {
            append: true,
            ..top("x = 1\n")
        };
        let err = absorb(Format::Toml, &layer, b"x = 1\n", b"x = 2\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("top layer is .append"), "{err}");
    }

    #[test]
    fn change_summary_format() {
        let key = |p: &str, f: Option<&str>, t: Option<&str>| Change::Key {
            path: p.into(),
            from: f.map(ToString::to_string),
            to: t.map(ToString::to_string),
        };
        let changes = vec![
            key("font-size", Some("20"), Some("22")),
            key("theme", None, Some("Nord")),
            key("old", Some("x"), None),
        ];
        assert_eq!(
            summary(&changes),
            "font-size: 20 -> 22, theme: added Nord, old: removed"
        );
        let many: Vec<Change> = (0..5)
            .map(|i| key(&format!("k{i}"), None, Some("v")))
            .collect();
        assert_eq!(summary(&many), "5 changes");
    }

    #[test]
    fn equivalent_ignores_formatting() {
        assert!(equivalent(Format::Toml, b"x = 1\n", b"x    =    1\n"));
        assert!(equivalent(Format::Ini, b"a = 1\n", b"a = 1\n\n# note\n"));
        assert!(!equivalent(Format::Text, b"a\n", b"a"));
    }
}
