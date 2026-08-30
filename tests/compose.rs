mod common;

use common::fixture_dir;
use rig::compose::desired;
use rig::repo::{load, select, Os};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    if !dir.is_dir() {
        return out;
    }
    for e in walkdir(dir) {
        out.insert(
            e.strip_prefix(dir).unwrap().to_path_buf(),
            std::fs::read(&e).unwrap(),
        );
    }
    out
}

fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walkdir(&p));
        } else {
            out.push(p);
        }
    }
    out
}

fn check(host: &str, os_spec: &str, expected: &str) {
    let root = fixture_dir("basic");
    let os = Os::parse(os_spec);
    let repo = load(&root).unwrap();
    let sel = select(&repo, host, &os).unwrap();
    let items = desired(&repo, &sel, &os).unwrap();

    let want = tree(&root.join("expected").join(expected));
    let got: BTreeMap<PathBuf, Vec<u8>> = items
        .iter()
        .map(|d| (d.target.module_path(), d.content.clone()))
        .collect();

    for (path, content) in &want {
        let have = got
            .get(path)
            .unwrap_or_else(|| panic!("{expected}: missing target {}", path.display()));
        assert_eq!(
            String::from_utf8_lossy(have),
            String::from_utf8_lossy(content),
            "{expected}: {} differs",
            path.display()
        );
    }
    let extra: Vec<_> = got.keys().filter(|k| !want.contains_key(*k)).collect();
    assert!(extra.is_empty(), "{expected}: extra targets {extra:?}");
}

#[test]
fn basic_composes_for_desktop_arch() {
    check("desktop", "linux:arch", "desktop-arch");
}

#[test]
fn basic_composes_for_macbook_macos() {
    check("macbook", "macos", "macbook-macos");
}

#[test]
fn modes_come_from_the_repo_file() {
    let root = fixture_dir("basic");
    let os = Os::parse("linux:arch");
    let repo = load(&root).unwrap();
    let sel = select(&repo, "desktop", &os).unwrap();
    let items = desired(&repo, &sel, &os).unwrap();
    let by_target: BTreeMap<String, &rig::compose::Desired> =
        items.iter().map(|d| (d.target.to_string(), d)).collect();
    assert_eq!(by_target["~/.local/bin/clip"].mode, 0o755);
    assert_eq!(by_target["~/.config/ghostty/config"].mode, 0o644);
}

#[test]
fn doctor_reports_a_broken_module_toml() {
    let sb = common::Sandbox::with_fixture("basic");
    sb.write_repo(
        "modules/hypr/module.toml",
        "when.os = [\"linux\"]\n\nbefore = \"nope\"\n",
    );
    let run = sb.rig(&["doctor", sb.repo.to_str().unwrap()]);
    assert_eq!(run.status, 2, "{}", run.stderr);
    assert!(run.stderr.contains("module.toml:3:"), "{}", run.stderr);
}

#[test]
fn compose_cli_writes_the_expected_tree() {
    for (host, os, expected) in [
        ("desktop", "linux:arch", "desktop-arch"),
        ("macbook", "macos", "macbook-macos"),
    ] {
        let sb = common::Sandbox::with_fixture("basic");
        let out = sb.root.path().join("out");
        let run = sb.rig(&[
            "compose",
            "--host",
            host,
            "--os",
            os,
            "--out",
            out.to_str().unwrap(),
            sb.repo.to_str().unwrap(),
        ]);
        assert_eq!(run.status, 0, "{}", run.stderr);
        assert_eq!(
            tree(&out),
            tree(&fixture_dir("basic").join("expected").join(expected))
        );
    }
}

#[test]
fn compose_cli_prints_every_target() {
    let sb = common::Sandbox::with_fixture("basic");
    let run = sb.rig(&[
        "compose",
        "--host",
        "desktop",
        "--os",
        "linux:arch",
        sb.repo.to_str().unwrap(),
    ]);
    assert_eq!(run.status, 0, "{}", run.stderr);
    assert!(
        run.stdout.contains("=== ~/.config/ghostty/config"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("=== /etc/modprobe.d/nvidia.conf"),
        "{}",
        run.stdout
    );
}

#[test]
fn doctor_warns_about_a_variant_nothing_matches() {
    let sb = common::Sandbox::with_fixture("basic");
    sb.write_repo(
        "modules/ghostty/home/.config/ghostty/config@laptop",
        "font-size = 14\n",
    );
    let run = sb.rig(&["doctor", sb.repo.to_str().unwrap()]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stderr.contains("variant 'laptop'"), "{}", run.stderr);
}

#[test]
fn doctor_warns_about_ini_sections_differing_only_by_case() {
    let sb = common::Sandbox::with_fixture("basic");
    sb.write_repo(
        "modules/git/home/.config/git/config",
        "[user]\n\tname = A\n[User]\n\temail = b\n",
    );
    let run = sb.rig(&["doctor", sb.repo.to_str().unwrap()]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stderr.contains("differ only by case"), "{}", run.stderr);
}

#[test]
fn doctor_is_clean_on_the_fixture() {
    let sb = common::Sandbox::with_fixture("basic");
    let run = sb.rig(&["doctor", sb.repo.to_str().unwrap()]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
}
