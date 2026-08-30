mod common;

use common::Sandbox;

const HOOKRUN: &str = "#!/bin/sh\nprintf 'hook %s\\n' \"$*\" >> \"$FAKE_LOG\"\n";
const HOOKFAIL: &str = "#!/bin/sh\nprintf 'hook %s\\n' \"$*\" >> \"$FAKE_LOG\"\nexit 1\n";

fn sandbox() -> Sandbox {
    let sb = Sandbox::with_fixture("hooks").with_os("linux:arch");
    sb.fake_bin("hookrun", HOOKRUN);
    sb
}

fn up(sb: &Sandbox, extra: &[&str]) -> common::Run {
    let repo = sb.repo.to_str().unwrap().to_string();
    let mut args = vec!["up", "--host", "box", "-y", &repo];
    args.extend_from_slice(extra);
    sb.rig(&args)
}

#[test]
fn changed_hook_runs_on_create_not_on_noop() {
    let sb = sandbox();
    let run = up(&sb, &[]);
    assert!(sb.log().contains(&"hook a".to_string()), "{:?}", sb.log());
    assert!(run.stdout.contains("hook"), "{}", run.stdout);

    std::fs::write(sb.log_path(), "").unwrap();
    up(&sb, &[]);
    assert!(!sb.log().contains(&"hook a".to_string()), "{:?}", sb.log());
}

#[test]
fn always_hook_runs_every_time() {
    let sb = sandbox();
    up(&sb, &[]);
    std::fs::write(sb.log_path(), "").unwrap();
    up(&sb, &[]);
    assert!(sb.log().contains(&"hook b".to_string()), "{:?}", sb.log());
}

#[test]
fn hook_env_has_changed_paths() {
    let sb = sandbox();
    sb.fake_bin(
        "hookrun",
        "#!/bin/sh\nprintf '%s' \"$RIG_CHANGED\" > \"$FAKE_LOG.$RIG_MODULE.env\"\nprintf '%s\\n' \"$RIG_MODULE $RIG_HOST\" >> \"$FAKE_LOG\"\n",
    );
    up(&sb, &[]);
    let changed = std::fs::read_to_string(format!("{}.a.env", sb.log_path().display())).unwrap();
    assert!(changed.contains(".config/a.conf"), "{changed}");
    assert!(sb.log().iter().any(|l| l == "a box"), "{:?}", sb.log());
}

#[test]
fn failed_hook_exit_2_others_still_run() {
    let sb = sandbox();
    sb.write_repo(
        "modules/a/module.toml",
        "[[hook]]\nafter = \"hookfail a\"\n",
    );
    sb.fake_bin("hookfail", HOOKFAIL);
    let run = up(&sb, &[]);
    assert_eq!(run.status, 2, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("FAILED (exit 1)"), "{}", run.stdout);
    assert!(sb.log().contains(&"hook b".to_string()), "{:?}", sb.log());
}

#[test]
fn dry_run_lists_hooks() {
    let sb = sandbox();
    let run = up(&sb, &["-n"]);
    assert!(run.stdout.contains("would run hookrun a"), "{}", run.stdout);
    assert!(sb.log().is_empty(), "{:?}", sb.log());
}

#[test]
fn edited_file_does_not_trigger_hook() {
    let sb = sandbox();
    up(&sb, &[]);
    sb.write_home(".config/a.conf", "edited by hand\n");
    std::fs::write(sb.log_path(), "").unwrap();
    let run = up(&sb, &[]);
    assert_eq!(run.outcome("~/.config/a.conf").unwrap(), "edited");
    assert!(!sb.log().contains(&"hook a".to_string()), "{:?}", sb.log());
}

#[test]
fn order_files_packages_hooks() {
    let sb = sandbox();
    sb.write_repo(
        "modules/a/module.toml",
        "[packages]\narch = [\"ghostty\"]\n\n[[hook]]\nafter = \"hookrun a\"\n",
    );
    sb.fake_bin(
        "pacman",
        "#!/bin/sh\n[ \"$1\" = \"-Qq\" ] && exit 0\n[ \"$1\" = \"-Sp\" ] && exit 0\n[ -f \"$HOME/.config/a.conf\" ] && printf 'file-present\\n' >> \"$FAKE_LOG\"\nprintf 'pacman %s\\n' \"$*\" >> \"$FAKE_LOG\"\n",
    );
    sb.fake_bin(
        "sudo",
        "#!/bin/sh\n[ \"$1\" = \"-n\" ] && shift\n[ \"$1\" = \"--\" ] && shift\nexec \"$@\"\n",
    );
    up(&sb, &[]);
    let log = sb.log();
    let file = log.iter().position(|l| l == "file-present").unwrap();
    let hook = log.iter().position(|l| l == "hook a").unwrap();
    assert!(file < hook, "{log:?}");
}

const FAKE_SUDO: &str = r#"#!/bin/sh
[ "$1" = "-n" ] && shift
[ "$1" = "--" ] && shift
exec "$@"
"#;

#[test]
fn etc_write_triggers_the_modules_hook() {
    let sb = sandbox();
    sb.fake_bin("sudo", FAKE_SUDO);
    sb.write_repo("modules/a/etc/rig-test.conf", "x\n");
    let etc_root = sb.etc_root().to_str().unwrap().to_string();
    let run = sb.rig(&[
        "up",
        "--host",
        "box",
        "-y",
        "--etc-root",
        &etc_root,
        sb.repo.to_str().unwrap(),
    ]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(sb.etc_root().join("etc/rig-test.conf").exists());
    assert!(sb.log().contains(&"hook a".to_string()), "{:?}", sb.log());
}

#[test]
fn only_an_etc_write_still_triggers_the_hook() {
    let sb = sandbox();
    sb.fake_bin("sudo", FAKE_SUDO);
    sb.write_repo("modules/a/etc/rig-test.conf", "x\n");
    let etc_root = sb.etc_root().to_str().unwrap().to_string();
    let args: Vec<&str> = vec![
        "up",
        "--host",
        "box",
        "-y",
        "--etc-root",
        &etc_root,
        sb.repo.to_str().unwrap(),
    ];
    sb.rig(&args);
    // Home is settled; only the etc file moves.
    sb.write_repo("modules/a/etc/rig-test.conf", "y\n");
    std::fs::write(sb.log_path(), "").unwrap();
    let run = sb.rig(&args);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(sb.log().contains(&"hook a".to_string()), "{:?}", sb.log());
}

#[test]
fn verbose_shows_hook_output() {
    let sb = sandbox();
    sb.fake_bin("hookrun", "#!/bin/sh\necho reloaded-$1\n");
    let run = sb.rig(&["up", "--host", "box", "-y", "-v", sb.repo.to_str().unwrap()]);
    assert!(run.stdout.contains("reloaded-a"), "{}", run.stdout);
}
