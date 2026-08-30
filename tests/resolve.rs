mod common;

use common::Sandbox;

const GHOSTTY: &str = ".config/ghostty/config";
const GHOSTTY_TARGET: &str = "~/.config/ghostty/config";
const GHOSTTY_REPO: &str = "modules/ghostty/home/.config/ghostty/config";

const STRIP_MARKERS: &str = r#"#!/bin/sh
grep -v -e '^<<<<<<<' -e '^|||||||' -e '^=======' -e '^>>>>>>>' -e '^theme = JetBrains' "$1" > "$1.tmp"
mv "$1.tmp" "$1"
"#;

const NOOP_EDITOR: &str = "#!/bin/sh\nexit 0\n";

fn ghostty(theme: &str) -> String {
    format!(
        "theme = {theme}\nfont-size = 20\nshell-integration-features = true\nkeybind = global:cmd+backquote=toggle_quick_terminal\nasync-backend = epoll\n"
    )
}

/// Drives both sides of the same line into a cell 9 conflict.
fn conflicted(editor: &str) -> Sandbox {
    let sb = Sandbox::with_fixture("basic").with_os("linux:ubuntu");
    sb.fake_bin("fake-editor", editor);
    sb.rig(&["up", "--host", "desktop", sb.repo.to_str().unwrap()]);
    sb.write_home(GHOSTTY, &ghostty("Mine"));
    sb.write_repo(GHOSTTY_REPO, &ghostty("Theirs"));
    let run = sb.rig(&["up"]);
    assert_eq!(run.outcome(GHOSTTY_TARGET).unwrap(), "conflict");
    sb
}

fn resolve(sb: &Sandbox, args: &[&str]) -> common::Run {
    let editor = sb.root.path().join("bin/fake-editor");
    let mut all = vec!["resolve"];
    all.extend_from_slice(args);
    let mut run = std::process::Command::new(env!("CARGO_BIN_EXE_rig"));
    run.args(&all)
        .current_dir(sb.root.path())
        .env("HOME", &sb.home)
        .env("XDG_CONFIG_HOME", sb.root.path().join("config"))
        .env("XDG_STATE_HOME", sb.root.path().join("state"))
        .env("RIG_OS", &sb.os)
        .env("EDITOR", &editor);
    let out = run.output().unwrap();
    common::Run {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn resolve_with_fake_editor() {
    let sb = conflicted(STRIP_MARKERS);
    let run = resolve(&sb, &[]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("resolved"), "{}", run.stdout);
    assert!(run.stdout.contains("run rig absorb"), "{}", run.stdout);

    let disk = sb.read_home(GHOSTTY);
    assert!(
        disk.contains("theme = Mine") && disk.contains("theme = Theirs"),
        "{disk}"
    );
    assert!(!sb.state_dir().join("conflicts/home").join(GHOSTTY).exists());

    assert_eq!(
        sb.rig(&["status"]).outcome(GHOSTTY_TARGET).unwrap(),
        "edited"
    );
    let absorbed = sb.rig(&["absorb", GHOSTTY_TARGET]);
    assert_eq!(absorbed.status, 0, "{}{}", absorbed.stdout, absorbed.stderr);
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
}

#[test]
fn resolve_leaves_markers() {
    let sb = conflicted(NOOP_EDITOR);
    let run = resolve(&sb, &[]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("markers remain"), "{}", run.stdout);
    assert!(sb.state_dir().join("conflicts/home").join(GHOSTTY).exists());
    assert_eq!(sb.read_home(GHOSTTY), ghostty("Mine"));
}

const FAKE_SUDO: &str = r#"#!/bin/sh
printf 'sudo %s\n' "$*" >> "$FAKE_LOG"
[ "$1" = "-n" ] && shift
[ "$1" = "--" ] && shift
exec "$@"
"#;

#[test]
fn resolve_etc_conflict_escalates() {
    let sb = Sandbox::with_fixture("basic").with_os("linux:arch");
    sb.fake_bin("sudo", FAKE_SUDO);
    sb.fake_bin("fake-editor", STRIP_MARKERS);
    let etc_root = sb.etc_root().to_str().unwrap().to_string();
    sb.rig(&[
        "up",
        "--host",
        "desktop",
        "--etc-root",
        &etc_root,
        sb.repo.to_str().unwrap(),
    ]);

    let conf = "etc/modprobe.d/nvidia.conf";
    std::fs::write(sb.etc_root().join(conf), "options nvidia_drm modeset=0\n").unwrap();
    sb.write_repo(
        "modules/nvidia/etc/modprobe.d/nvidia.conf",
        "options nvidia_drm modeset=2\n",
    );
    let up = sb.rig(&[
        "up",
        "--host",
        "desktop",
        "--etc-root",
        &etc_root,
        sb.repo.to_str().unwrap(),
    ]);
    assert_eq!(
        up.outcome("/etc/modprobe.d/nvidia.conf").unwrap(),
        "conflict",
        "{}",
        up.stdout
    );

    std::fs::write(sb.log_path(), "").unwrap();
    let editor = sb.root.path().join("bin/fake-editor");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_rig"))
        .args(["resolve", "--etc-root", &etc_root])
        .current_dir(sb.root.path())
        .env("HOME", &sb.home)
        .env("XDG_CONFIG_HOME", sb.root.path().join("config"))
        .env("XDG_STATE_HOME", sb.root.path().join("state"))
        .env("RIG_OS", &sb.os)
        .env("EDITOR", &editor)
        .env("FAKE_LOG", sb.log_path())
        .env(
            "PATH",
            format!(
                "{}:{}",
                sb.root.path().join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let logged = sb.log().join("\n");
    assert!(logged.contains("resolve --etc-only"), "{logged}");
    assert!(logged.contains("--host desktop"), "{logged}");
    let resolved = std::fs::read_to_string(sb.etc_root().join(conf)).unwrap();
    assert!(
        resolved.contains("modeset=0") && resolved.contains("modeset=2"),
        "{resolved}"
    );
}
