mod common;

use common::Sandbox;

const FAKE: &str = r#"#!/bin/sh
printf 'defaults %s\n' "$*" >> "$FAKE_LOG"
touch "$FAKE_DEFAULTS"
case "$1" in
  read)
    v=$(sed -n "s/^$2 $3 [^ ]* //p" "$FAKE_DEFAULTS")
    [ -z "$v" ] && { echo "The domain/default pair of ($2, $3) does not exist" >&2; exit 1; }
    echo "$v" ;;
  read-type)
    case $(sed -n "s/^$2 $3 \([^ ]*\) .*/\1/p" "$FAKE_DEFAULTS") in
      -int) echo "Type is integer" ;;
      -bool) echo "Type is boolean" ;;
      -float) echo "Type is float" ;;
      *) echo "Type is string" ;;
    esac ;;
  write)
    grep -v "^$2 $3 " "$FAKE_DEFAULTS" > "$FAKE_DEFAULTS.t"; mv "$FAKE_DEFAULTS.t" "$FAKE_DEFAULTS"
    echo "$2 $3 $4 $5" >> "$FAKE_DEFAULTS" ;;
  delete)
    grep -v "^$2 $3 " "$FAKE_DEFAULTS" > "$FAKE_DEFAULTS.t"; mv "$FAKE_DEFAULTS.t" "$FAKE_DEFAULTS" ;;
esac
exit 0
"#;

const MODULE: &str = "when.os = [\"macos\"]\n\n[defaults.NSGlobalDomain]\nInitialKeyRepeat = 15\nKeyRepeat = 1\nApplePressAndHoldEnabled = false\n";

fn sandbox(os: &str) -> Sandbox {
    let mut sb = Sandbox::with_fixture("hooks").with_os(os);
    sb.fake_bin("defaults", FAKE);
    sb.write_repo("modules/a/module.toml", MODULE);
    sb.write_repo("hosts/box.toml", "modules = [\"a\"]\n");
    let db = sb.root.path().join("fake.defaults");
    sb.set_env("FAKE_DEFAULTS", db.to_str().unwrap());
    sb
}

/// Sets a value the way the user would, typed by its shape.
fn set_machine(sb: &Sandbox, domain: &str, key: &str, value: &str) {
    let flag = if value.parse::<i64>().is_ok() {
        "-int"
    } else if value.parse::<f64>().is_ok() {
        "-float"
    } else {
        "-string"
    };
    let db = sb.root.path().join("fake.defaults");
    let prefix = format!("{domain} {key} {flag} ");
    let mut lines: Vec<String> = std::fs::read_to_string(&db)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with(&prefix))
        .map(ToString::to_string)
        .collect();
    lines.push(format!("{prefix}{value}"));
    std::fs::write(db, lines.join("\n") + "\n").unwrap();
}

fn up(sb: &Sandbox, extra: &[&str]) -> common::Run {
    let repo = sb.repo.to_str().unwrap().to_string();
    let mut args = vec!["up", "--host", "box", "-y", &repo];
    args.extend_from_slice(extra);
    sb.rig(&args)
}

fn writes(sb: &Sandbox) -> Vec<String> {
    sb.log()
        .into_iter()
        .filter(|l| l.starts_with("defaults write"))
        .collect()
}

#[test]
fn first_up_sets_then_idles() {
    let sb = sandbox("macos");
    let run = up(&sb, &[]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        writes(&sb),
        vec![
            "defaults write NSGlobalDomain ApplePressAndHoldEnabled -bool 0",
            "defaults write NSGlobalDomain InitialKeyRepeat -int 15",
            "defaults write NSGlobalDomain KeyRepeat -int 1",
        ]
    );
    assert_eq!(run.stdout.matches("(set)").count(), 3, "{}", run.stdout);
    let m = std::fs::read_to_string(sb.state_dir().join("manifest.toml")).unwrap();
    assert!(
        m.contains(
            "[settings.\"macos:NSGlobalDomain.InitialKeyRepeat\"]\ncurrent = \"15\"\nunset = true"
        ),
        "{m}"
    );

    std::fs::write(sb.log_path(), "").unwrap();
    let run = up(&sb, &[]);
    assert!(writes(&sb).is_empty(), "{:?}", sb.log());
    assert!(!run.stdout.contains("setting"), "{}", run.stdout);
    let run = up(&sb, &["-v"]);
    assert!(run.stdout.contains("macos: 3 ok"), "{}", run.stdout);
}

#[test]
fn matching_value_is_adopted_quietly() {
    let sb = sandbox("macos");
    set_machine(&sb, "NSGlobalDomain", "InitialKeyRepeat", "15");
    let run = up(&sb, &[]);
    assert!(!run.stdout.contains("InitialKeyRepeat"), "{}", run.stdout);
    let run = up(&sb, &["-v"]);
    assert!(run.stdout.contains("macos: 3 ok"), "{}", run.stdout);
    assert!(
        writes(&sb).iter().all(|w| !w.contains("InitialKeyRepeat")),
        "{:?}",
        writes(&sb)
    );
}

#[test]
fn repo_change_updates() {
    let sb = sandbox("macos");
    up(&sb, &[]);
    sb.write_repo("modules/a/module.toml", &MODULE.replace("= 15", "= 20"));
    std::fs::write(sb.log_path(), "").unwrap();
    let run = up(&sb, &[]);
    assert_eq!(
        writes(&sb),
        vec!["defaults write NSGlobalDomain InitialKeyRepeat -int 20"]
    );
    assert!(run.stdout.contains("(updated)"), "{}", run.stdout);
}

#[test]
fn edited_is_left_alone_until_forced() {
    let sb = sandbox("macos");
    up(&sb, &[]);
    set_machine(&sb, "NSGlobalDomain", "InitialKeyRepeat", "12");

    let run = sb.rig(&["status", "--host", "box"]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("setting")
            && run.stdout.contains("NSGlobalDomain.InitialKeyRepeat")
            && run.stdout.contains("(edited: 12, repo: 15)"),
        "{}",
        run.stdout
    );

    let run = sb.rig(&["diff", "--host", "box"]);
    assert!(
        run.stdout
            .contains("NSGlobalDomain.InitialKeyRepeat: 12 -> 15"),
        "{}",
        run.stdout
    );

    std::fs::write(sb.log_path(), "").unwrap();
    let run = up(&sb, &[]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(writes(&sb).is_empty(), "{:?}", sb.log());
    assert!(
        run.stdout.contains("(edited: 12, repo: 15)"),
        "{}",
        run.stdout
    );

    let run = up(&sb, &["--force"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        writes(&sb),
        vec!["defaults write NSGlobalDomain InitialKeyRepeat -int 15"]
    );
}

#[test]
fn types_round_trip() {
    let sb = sandbox("macos");
    sb.write_repo(
        "modules/a/module.toml",
        "[defaults.\"com.apple.dock\"]\ntilesize = 48\nautohide = true\norientation = \"left\"\nscale = 1.5\n",
    );
    let run = up(&sb, &[]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        writes(&sb),
        vec![
            "defaults write com.apple.dock autohide -bool 1",
            "defaults write com.apple.dock orientation -string left",
            "defaults write com.apple.dock scale -float 1.5",
            "defaults write com.apple.dock tilesize -int 48",
        ]
    );
    set_machine(&sb, "com.apple.dock", "scale", "1.50");
    std::fs::write(sb.log_path(), "").unwrap();
    up(&sb, &[]);
    assert!(writes(&sb).is_empty(), "{:?}", sb.log());
}

#[test]
fn dropped_key_restores_the_original() {
    let sb = sandbox("macos");
    set_machine(&sb, "NSGlobalDomain", "InitialKeyRepeat", "25");
    up(&sb, &[]);
    let m = std::fs::read_to_string(sb.state_dir().join("manifest.toml")).unwrap();
    assert!(
        m.contains("[settings.\"macos:NSGlobalDomain.InitialKeyRepeat\".original]\nvalue = \"25\"\nkind = \"integer\""),
        "{m}"
    );

    sb.write_repo(
        "modules/a/module.toml",
        "[defaults.NSGlobalDomain]\nKeyRepeat = 1\n",
    );
    std::fs::write(sb.log_path(), "").unwrap();
    let run = up(&sb, &[]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout
            .contains("restored   NSGlobalDomain.InitialKeyRepeat")
            && run
                .stdout
                .contains("restored   NSGlobalDomain.ApplePressAndHoldEnabled"),
        "{}",
        run.stdout
    );
    assert_eq!(
        writes(&sb),
        vec!["defaults write NSGlobalDomain InitialKeyRepeat -int 25"]
    );
    assert!(
        sb.log()
            .contains(&"defaults delete NSGlobalDomain ApplePressAndHoldEnabled".to_string()),
        "{:?}",
        sb.log()
    );
    let m = std::fs::read_to_string(sb.state_dir().join("manifest.toml")).unwrap();
    assert!(!m.contains("InitialKeyRepeat"), "{m}");
    assert!(!m.contains("ApplePressAndHoldEnabled"), "{m}");
    assert!(m.contains("KeyRepeat"), "{m}");

    std::fs::write(sb.log_path(), "").unwrap();
    let again = up(&sb, &[]);
    assert_eq!(again.stdout, "nothing to do\n");
    assert!(writes(&sb).is_empty(), "{:?}", sb.log());
}

#[test]
fn legacy_ledger_entry_is_reported_and_kept() {
    let sb = sandbox("macos");
    up(&sb, &[]);
    let path = sb.state_dir().join("manifest.toml");
    let m = std::fs::read_to_string(&path).unwrap();
    let start = m
        .find("[settings.\"macos:NSGlobalDomain.InitialKeyRepeat\"]")
        .unwrap();
    let end = m[start + 1..]
        .find("\n[")
        .map_or(m.len(), |i| start + 1 + i);
    let m = format!(
        "{}[settings]\n\"macos:NSGlobalDomain.InitialKeyRepeat\" = \"15\"\n{}",
        &m[..start],
        &m[end..]
    );
    std::fs::write(&path, m).unwrap();

    sb.write_repo(
        "modules/a/module.toml",
        "[defaults.NSGlobalDomain]\nKeyRepeat = 1\nApplePressAndHoldEnabled = false\n",
    );
    std::fs::write(sb.log_path(), "").unwrap();
    let run = up(&sb, &[]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout
            .contains("orphaned   NSGlobalDomain.InitialKeyRepeat")
            && run.stdout.contains("original unknown, kept"),
        "{}",
        run.stdout
    );
    assert!(writes(&sb).is_empty(), "{:?}", sb.log());
    let m = std::fs::read_to_string(&path).unwrap();
    assert!(!m.contains("InitialKeyRepeat"), "{m}");
}

#[test]
fn dry_run_writes_nothing() {
    let sb = sandbox("macos");
    let run = up(&sb, &["-n"]);
    assert!(run.stdout.contains("would set"), "{}", run.stdout);
    assert!(writes(&sb).is_empty(), "{:?}", sb.log());
    assert!(!sb.state_dir().join("manifest.toml").exists());
}

#[test]
fn dry_run_and_missing_tool_keep_the_ledger() {
    let sb = sandbox("macos");
    up(&sb, &[]);
    let manifest = || std::fs::read_to_string(sb.state_dir().join("manifest.toml")).unwrap();
    let before = manifest();
    sb.write_repo(
        "modules/a/module.toml",
        "[defaults.NSGlobalDomain]\nKeyRepeat = 1\n",
    );
    up(&sb, &["-n"]);
    assert_eq!(manifest(), before);
    sb.remove_fake_bin("defaults");
    up(&sb, &[]);
    assert_eq!(manifest(), before);
}

#[test]
fn inert_on_linux() {
    let sb = sandbox("linux:arch");
    let run = up(&sb, &[]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(sb.log().is_empty(), "{:?}", sb.log());
}

#[test]
fn missing_defaults_tool_is_skipped() {
    let sb = sandbox("macos");
    sb.remove_fake_bin("defaults");
    let run = up(&sb, &[]);
    assert!(run.stdout.contains("settings: macos"), "{}", run.stdout);
}

#[test]
fn non_scalar_is_a_load_error() {
    let sb = sandbox("macos");
    sb.write_repo("modules/a/module.toml", "[defaults.d]\nk = [1, 2]\n");
    let run = up(&sb, &[]);
    assert_eq!(run.status, 2, "{}", run.stderr);
    assert!(
        run.stderr.contains("defaults.d.k must be a scalar"),
        "{}",
        run.stderr
    );
}
