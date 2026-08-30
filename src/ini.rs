//! A tolerant INI parser that round trips untouched lines byte for byte.

/// One physical line, including its terminator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    Blank(String),
    Comment(String),
    Entry {
        key: String,
        value: String,
        raw: String,
    },
}

impl Line {
    fn raw(&self) -> &str {
        match self {
            Line::Blank(r) | Line::Comment(r) => r,
            Line::Entry { raw, .. } => raw,
        }
    }
}

/// A `[header]` and the lines under it. `name` is `None` before the first header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub name: Option<String>,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ini {
    pub sections: Vec<Section>,
}

fn is_key(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn raw_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, _) in text.match_indices('\n') {
        out.push(&text[start..=i]);
        start = i + 1;
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Parses INI text. `Err` carries the 1-based line number that failed.
///
/// # Errors
/// When a line is neither blank, comment, header nor `key[ = value]`.
pub fn parse(text: &str) -> Result<Ini, usize> {
    let mut ini = Ini::default();
    let mut cur = Section {
        name: None,
        lines: Vec::new(),
    };
    for (n, raw) in raw_lines(text).into_iter().enumerate() {
        let t = raw.trim();
        if t.is_empty() {
            cur.lines.push(Line::Blank(raw.to_string()));
        } else if t.starts_with('#') || t.starts_with(';') {
            cur.lines.push(Line::Comment(raw.to_string()));
        } else if t.starts_with('[') {
            if !t.ends_with(']') {
                return Err(n + 1);
            }
            if cur.name.is_some() || !cur.lines.is_empty() {
                ini.sections.push(cur);
            }
            cur = Section {
                name: Some(t[1..t.len() - 1].to_string()),
                lines: Vec::new(),
            };
        } else if let Some((k, v)) = t.split_once('=') {
            let key = k.trim();
            if !is_key(key) {
                return Err(n + 1);
            }
            cur.lines.push(Line::Entry {
                key: key.to_string(),
                value: v.trim().to_string(),
                raw: raw.to_string(),
            });
        } else if is_key(t) {
            cur.lines.push(Line::Entry {
                key: t.to_string(),
                value: String::new(),
                raw: raw.to_string(),
            });
        } else {
            return Err(n + 1);
        }
    }
    if cur.name.is_some() || !cur.lines.is_empty() {
        ini.sections.push(cur);
    }
    Ok(ini)
}

impl Ini {
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for s in &self.sections {
            if let Some(n) = &s.name {
                out.push('[');
                out.push_str(n);
                out.push_str("]\n");
            }
            for l in &s.lines {
                out.push_str(l.raw());
            }
        }
        out
    }

    /// Every entry as `(section, key, value)`, ignoring blanks and comments.
    #[must_use]
    pub fn entries(&self) -> Vec<(Option<&str>, &str, &str)> {
        let mut out = Vec::new();
        for s in &self.sections {
            for l in &s.lines {
                if let Line::Entry { key, value, .. } = l {
                    out.push((s.name.as_deref(), key.as_str(), value.as_str()));
                }
            }
        }
        out
    }

    fn section_mut(&mut self, name: Option<&str>) -> Option<&mut Section> {
        self.sections.iter_mut().find(|s| s.name.as_deref() == name)
    }

    #[allow(clippy::missing_panics_doc)]
    fn push_section(&mut self, name: Option<&str>) -> &mut Section {
        let section = Section {
            name: name.map(ToString::to_string),
            lines: Vec::new(),
        };
        if name.is_none() {
            self.sections.insert(0, section);
            return &mut self.sections[0];
        }
        ensure_trailing_newline(self);
        self.sections.push(section);
        self.sections.last_mut().unwrap()
    }

    /// All values for `key` in `section`, in file order.
    #[must_use]
    pub fn get(&self, section: Option<&str>, key: &str) -> Vec<&str> {
        self.sections
            .iter()
            .filter(|s| s.name.as_deref() == section)
            .flat_map(|s| &s.lines)
            .filter_map(|l| match l {
                Line::Entry { key: k, value, .. } if k == key => Some(value.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Replaces every occurrence of `key` in `section` with `values`, creating the section if needed.
    ///
    /// # Panics
    /// Never; the section is created just above.
    pub fn set(&mut self, section: Option<&str>, key: &str, values: &[String]) {
        if self.section_mut(section).is_none() {
            self.push_section(section);
        }
        let s = self.section_mut(section).unwrap();
        set_in_section(s, key, values);
    }

    /// Removes every occurrence of `key` in `section`. Returns whether anything was removed.
    pub fn remove(&mut self, section: Option<&str>, key: &str) -> bool {
        let Some(s) = self.section_mut(section) else {
            return false;
        };
        let before = s.lines.len();
        s.lines
            .retain(|l| !matches!(l, Line::Entry { key: k, .. } if k == key));
        before != s.lines.len()
    }
}

fn ensure_trailing_newline(ini: &mut Ini) {
    if let Some(l) = ini
        .sections
        .last_mut()
        .and_then(|s| s.lines.last_mut())
        .filter(|l| !l.raw().ends_with('\n'))
    {
        match l {
            Line::Blank(r) | Line::Comment(r) => r.push('\n'),
            Line::Entry { raw, .. } => raw.push('\n'),
        }
    }
}

fn indent_of(raw: &str) -> &str {
    &raw[..raw.len() - raw.trim_start().len()]
}

fn entry_positions(s: &Section, key: &str) -> Vec<usize> {
    s.lines
        .iter()
        .enumerate()
        .filter(|(_, l)| matches!(l, Line::Entry { key: k, .. } if k == key))
        .map(|(i, _)| i)
        .collect()
}

/// Index after the last non-blank line, so appends land before trailing blanks.
fn append_at(s: &Section) -> usize {
    s.lines
        .iter()
        .rposition(|l| !matches!(l, Line::Blank(_)))
        .map_or(0, |i| i + 1)
}

fn render_entry(indent: &str, key: &str, value: &str) -> String {
    if value.is_empty() {
        format!("{indent}{key}\n")
    } else {
        format!("{indent}{key} = {value}\n")
    }
}

fn set_in_section(s: &mut Section, key: &str, values: &[String]) {
    let pos = entry_positions(s, key);
    if let ([i], [v]) = (&pos[..], values) {
        let indent = indent_of(s.lines[*i].raw()).to_string();
        s.lines[*i] = Line::Entry {
            key: key.to_string(),
            value: v.clone(),
            raw: render_entry(&indent, key, v),
        };
        return;
    }
    let indent = pos
        .first()
        .map_or_else(String::new, |i| indent_of(s.lines[*i].raw()).to_string());
    for i in pos.into_iter().rev() {
        s.lines.remove(i);
    }
    let at = append_at(s);
    let new: Vec<Line> = values
        .iter()
        .map(|v| Line::Entry {
            key: key.to_string(),
            value: v.clone(),
            raw: render_entry(&indent, key, v),
        })
        .collect();
    s.lines.splice(at..at, new);
}

fn distinct_keys(s: &Section) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in &s.lines {
        if let Line::Entry { key, .. } = l {
            if !out.iter().any(|k| k == key) {
                out.push(key.clone());
            }
        }
    }
    out
}

/// Merges `from` over `into`, section by section and key by key.
///
/// # Panics
/// Never; sections are created before they are looked up.
pub fn merge(into: &mut Ini, from: &Ini) {
    for fs in &from.sections {
        if into.section_mut(fs.name.as_deref()).is_none() {
            into.push_section(fs.name.as_deref());
        }
        for key in distinct_keys(fs) {
            let f: Vec<Line> = fs
                .lines
                .iter()
                .filter(|l| matches!(l, Line::Entry { key: k, .. } if *k == key))
                .cloned()
                .collect();
            let is = into.section_mut(fs.name.as_deref()).unwrap();
            let ipos = entry_positions(is, &key);
            if let ([i], [Line::Entry { value, raw, .. }]) = (&ipos[..], &f[..]) {
                let indent = indent_of(is.lines[*i].raw()).to_string();
                let body = raw.trim_start().trim_end_matches(['\n', '\r']);
                is.lines[*i] = Line::Entry {
                    key: key.clone(),
                    value: value.clone(),
                    raw: format!("{indent}{body}\n"),
                };
            } else {
                for i in ipos.into_iter().rev() {
                    is.lines.remove(i);
                }
                let at = append_at(is);
                let new: Vec<Line> = f
                    .iter()
                    .map(|l| {
                        let Line::Entry { key, value, raw } = l else {
                            unreachable!()
                        };
                        let mut raw = raw.clone();
                        if !raw.ends_with('\n') {
                            raw.push('\n');
                        }
                        Line::Entry {
                            key: key.clone(),
                            value: value.clone(),
                            raw,
                        }
                    })
                    .collect();
                is.lines.splice(at..at, new);
            }
        }
    }
}

/// Section names that differ only by case, which git would treat as one section.
#[must_use]
pub fn case_collisions(ini: &Ini) -> Vec<(String, String)> {
    let names: Vec<&String> = ini
        .sections
        .iter()
        .filter_map(|s| s.name.as_ref())
        .collect();
    let mut out = Vec::new();
    for (i, a) in names.iter().enumerate() {
        for b in &names[i + 1..] {
            if a != b && a.to_lowercase() == b.to_lowercase() {
                out.push(((*a).clone(), (*b).clone()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merged(lower: &str, upper: &str) -> String {
        let mut a = parse(lower).unwrap();
        merge(&mut a, &parse(upper).unwrap());
        a.render()
    }

    #[test]
    fn ini_replace_single_key() {
        assert_eq!(
            merged("[user]\n\tsigningkey = a\n", "[user]\n\tsigningkey = b\n"),
            "[user]\n\tsigningkey = b\n"
        );
    }

    #[test]
    fn ini_duplicate_keys_append_as_group() {
        let out = merged(
            "theme = dark\nkeybind = a=b\n",
            "keybind = c=d\nkeybind = e=f\n",
        );
        assert_eq!(out, "theme = dark\nkeybind = c=d\nkeybind = e=f\n");
    }

    #[test]
    fn ini_new_section_appended() {
        assert_eq!(
            merged("[user]\n\tname = A\n", "[core]\n\teditor = hx\n"),
            "[user]\n\tname = A\n[core]\n\teditor = hx\n"
        );
    }

    #[test]
    fn ini_subsection_name_roundtrip() {
        let text = "[credential \"https://gist.github.com\"]\n\thelper = gh\n";
        let ini = parse(text).unwrap();
        assert_eq!(ini.render(), text);
        assert_eq!(
            ini.sections[0].name.as_deref(),
            Some("credential \"https://gist.github.com\"")
        );
    }

    #[test]
    fn ini_rejects_lua_and_fish() {
        assert!(parse("return {\n  x = 1,\n}\n").is_err());
        assert!(parse("set -gx EDITOR hx\n").is_err());
    }

    #[test]
    fn ini_case_collision_is_reported() {
        let ini = parse("[User]\n\tname = A\n[user]\n\temail = b\n").unwrap();
        assert_eq!(case_collisions(&ini).len(), 1);
    }

    #[test]
    fn ini_headless_keys_stay_above_the_first_header() {
        assert_eq!(merged("[a]\nx = 1\n", "y = 2\n"), "y = 2\n[a]\nx = 1\n");
    }

    #[test]
    fn ini_bare_key_stays_bare() {
        assert_eq!(
            merged("[core]\n\tbare\n", "[core]\n\tbare\n"),
            "[core]\n\tbare\n"
        );
    }

    #[test]
    fn ini_set_and_remove() {
        let mut ini = parse("[user]\n\tname = A\n").unwrap();
        ini.set(Some("user"), "name", &["B".into()]);
        ini.set(Some("core"), "editor", &["hx".into()]);
        assert_eq!(ini.render(), "[user]\n\tname = B\n[core]\neditor = hx\n");
        assert!(ini.remove(Some("user"), "name"));
        assert!(!ini.remove(Some("user"), "name"));
        assert_eq!(ini.get(Some("core"), "editor"), vec!["hx"]);
    }
}
