mod common;

use common::Sandbox;

const FAKE_SUDO: &str = r#"#!/bin/sh
printf 'sudo %s\n' "$*" >> "$FAKE_LOG"
[ "$1" = "-n" ] && shift
[ "$1" = "--" ] && shift
exec "$@"
"#;

const FAKE_SUDO_DENIES_N: &str = r#"#!/bin/sh
printf 'sudo %s\n' "$*" >> "$FAKE_LOG"
if [ "$1" = "-n" ]; then
  echo "sudo: a password is required" >&2
  exit 1
fi
shift
exec "$@"
"#;

/// A host with no `/etc` targets, so the home phase is the whole run.
fn home_only() -> Sandbox {
    Sandbox::with_fixture("basic").with_os("linux:ubuntu")
}

fn up(sb: &Sandbox, extra: &[&str]) -> common::Run {
    let repo = sb.repo.to_str().unwrap().to_string();
    let mut args = vec!["up", "--host", "desktop", &repo];
    args.extend_from_slice(extra);
    sb.rig(&args)
}

const GHOSTTY: &str = ".config/ghostty/config";
const GHOSTTY_TARGET: &str = "~/.config/ghostty/config";
const GHOSTTY_REPO: &str = "modules/ghostty/home/.config/ghostty/config";

fn ghostty(theme: &str, size: &str, backend: &str) -> String {
    format!(
        "theme = {theme}\nfont-size = {size}\nshell-integration-features = true\nkeybind = global:cmd+backquote=toggle_quick_terminal\nasync-backend = {backend}\n"
    )
}

#[test]
fn up_creates_everything_then_noop() {
    let sb = home_only();
    let first = up(&sb, &[]);
    assert_eq!(first.status, 0, "{}{}", first.stdout, first.stderr);
    assert_eq!(first.rows().len(), 9, "{}", first.stdout);
    assert!(first.rows().iter().all(|(o, _)| o == "created"));
    assert_eq!(sb.mode(".local/bin/clip"), 0o755);
    assert_eq!(sb.mode(GHOSTTY), 0o644);
    assert_eq!(
        sb.read_home(GHOSTTY),
        ghostty("JetBrains Darcula", "20", "epoll")
    );

    let second = sb.rig(&["up"]);
    assert_eq!(second.status, 0);
    assert_eq!(second.stdout, "nothing to do\n");
}

#[test]
fn up_is_fast_when_unchanged() {
    if std::env::var_os("CI").is_some() {
        return;
    }
    let sb = home_only();
    up(&sb, &[]);
    let start = std::time::Instant::now();
    sb.rig(&["up"]);
    assert!(start.elapsed().as_millis() < 100, "{:?}", start.elapsed());
}

#[test]
fn cell2_adopts_an_identical_file() {
    let sb = home_only();
    sb.write_home(GHOSTTY, &ghostty("JetBrains Darcula", "20", "epoll"));
    let run = up(&sb, &[]);
    assert_eq!(run.outcome(GHOSTTY_TARGET).unwrap(), "adopted");
    assert_eq!(run.status, 0);
}

#[test]
fn cell3_unmanaged_file_differs() {
    let sb = home_only();
    sb.write_home(GHOSTTY, "font-size = 99\n");
    let run = up(&sb, &[]);
    assert_eq!(run.outcome(GHOSTTY_TARGET).unwrap(), "conflict");
    assert_eq!(run.status, 1);
    assert_eq!(sb.read_home(GHOSTTY), "font-size = 99\n");

    let adopted = up(&sb, &["--adopt"]);
    assert_eq!(adopted.outcome(GHOSTTY_TARGET).unwrap(), "adopted");
    assert_eq!(
        sb.read_home(GHOSTTY),
        ghostty("JetBrains Darcula", "20", "epoll")
    );
}

#[test]
fn adopt_keeps_the_previous_content_as_a_blob() {
    let sb = home_only();
    sb.write_home(GHOSTTY, "font-size = 99\n");
    let run = up(&sb, &["--adopt"]);
    let note = run
        .stdout
        .lines()
        .find(|l| l.contains(GHOSTTY_TARGET))
        .unwrap();
    let hash8 = note.rsplit(' ').next().unwrap();
    let blobs: Vec<String> = std::fs::read_dir(sb.state_dir().join("blobs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let kept = blobs.iter().find(|b| b.starts_with(hash8)).unwrap();
    assert_eq!(
        std::fs::read_to_string(sb.state_dir().join("blobs").join(kept)).unwrap(),
        "font-size = 99\n"
    );

    // One more run and it is collected, as documented.
    sb.rig(&["up"]);
    assert!(!sb.state_dir().join("blobs").join(kept).exists());
}

#[test]
fn dry_run_says_so() {
    let sb = home_only();
    let run = up(&sb, &["-n"]);
    assert!(run.stdout.starts_with("(dry run)\n"), "{}", run.stdout);
    assert!(run.stdout.contains("would write"), "{}", run.stdout);
}

#[test]
fn cell2_adoption_keeps_the_repo_mode() {
    let sb = home_only();
    up(&sb, &[]);
    std::fs::remove_dir_all(sb.state_dir()).unwrap();
    sb.rig(&["up"]);
    let manifest = std::fs::read_to_string(sb.state_dir().join("manifest.toml")).unwrap();
    let clip = manifest
        .split("[targets.\"~/.local/bin/clip\"]")
        .nth(1)
        .unwrap();
    assert!(clip.contains("mode = \"0755\""), "{clip}");
}

#[test]
fn cell4_deleted_on_disk_and_force_recreates() {
    let sb = home_only();
    up(&sb, &[]);
    std::fs::remove_file(sb.home.join(GHOSTTY)).unwrap();
    let run = sb.rig(&["up"]);
    assert_eq!(run.outcome(GHOSTTY_TARGET).unwrap(), "deleted");
    assert_eq!(run.status, 1);
    assert!(!sb.home_exists(GHOSTTY));

    let forced = sb.rig(&["up", "--force", GHOSTTY_TARGET]);
    assert_eq!(forced.outcome(GHOSTTY_TARGET).unwrap(), "created");
    assert!(sb.home_exists(GHOSTTY));
}

#[test]
fn cell6_repo_change_updates_disk() {
    let sb = home_only();
    up(&sb, &[]);
    sb.write_repo(GHOSTTY_REPO, &ghostty("JetBrains Darcula", "24", "epoll"));
    let run = sb.rig(&["up"]);
    assert_eq!(run.outcome(GHOSTTY_TARGET).unwrap(), "updated");
    assert!(sb.read_home(GHOSTTY).contains("font-size = 24"));
}

#[test]
fn cell7_edited_is_never_overwritten() {
    let sb = home_only();
    up(&sb, &[]);
    sb.write_home(GHOSTTY, &ghostty("JetBrains Darcula", "22", "epoll"));
    let run = sb.rig(&["up"]);
    assert_eq!(run.outcome(GHOSTTY_TARGET).unwrap(), "edited");
    assert_eq!(run.status, 1);
    assert!(sb.read_home(GHOSTTY).contains("font-size = 22"));

    let forced = sb.rig(&["up", "--force"]);
    assert_eq!(forced.outcome(GHOSTTY_TARGET).unwrap(), "updated");
    assert!(sb.read_home(GHOSTTY).contains("font-size = 20"));
}

#[test]
fn cell8_then_up_again_reports_edited() {
    let sb = home_only();
    up(&sb, &[]);
    sb.write_home(GHOSTTY, &ghostty("JetBrains Darcula", "20", "io_uring"));
    sb.write_repo(GHOSTTY_REPO, &ghostty("Nord", "20", "epoll"));
    let run = sb.rig(&["up"]);
    assert_eq!(run.outcome(GHOSTTY_TARGET).unwrap(), "merged");
    assert_eq!(sb.read_home(GHOSTTY), ghostty("Nord", "20", "io_uring"));

    // State recorded the repo's version, so the edit survives as cell 7.
    let again = sb.rig(&["up"]);
    assert_eq!(again.outcome(GHOSTTY_TARGET).unwrap(), "edited");
    assert_eq!(sb.read_home(GHOSTTY), ghostty("Nord", "20", "io_uring"));
}

#[test]
fn cell9_conflict_writes_markers_and_leaves_disk_alone() {
    let sb = home_only();
    up(&sb, &[]);
    sb.write_home(GHOSTTY, &ghostty("Mine", "20", "epoll"));
    sb.write_repo(GHOSTTY_REPO, &ghostty("Theirs", "20", "epoll"));
    let run = sb.rig(&["up"]);
    assert_eq!(run.outcome(GHOSTTY_TARGET).unwrap(), "conflict");
    assert_eq!(run.status, 1);
    assert_eq!(sb.read_home(GHOSTTY), ghostty("Mine", "20", "epoll"));
    let marked =
        std::fs::read_to_string(sb.state_dir().join("conflicts/home").join(GHOSTTY)).unwrap();
    assert!(marked.contains("<<<<<<<") && marked.contains("Mine") && marked.contains("Theirs"));

    // Resolving the repo side clears the conflict file on the next run.
    sb.write_repo(GHOSTTY_REPO, &ghostty("Mine", "20", "epoll"));
    sb.rig(&["up"]);
    assert!(!sb.state_dir().join("conflicts/home").join(GHOSTTY).exists());
}

#[test]
fn status_exit_code_1_on_drift_and_0_when_clean() {
    let sb = home_only();
    up(&sb, &[]);
    let clean = sb.rig(&["status"]);
    assert_eq!(clean.status, 0);
    assert_eq!(clean.stdout, "nothing to do\n");

    sb.write_home(GHOSTTY, &ghostty("JetBrains Darcula", "22", "epoll"));
    let drift = sb.rig(&["status"]);
    assert_eq!(drift.status, 1);
    assert_eq!(drift.outcome(GHOSTTY_TARGET).unwrap(), "edited");
    // status never writes.
    assert!(sb.read_home(GHOSTTY).contains("font-size = 22"));
}

#[test]
fn diff_prints_patch_for_edited() {
    let sb = home_only();
    up(&sb, &[]);
    sb.write_home(GHOSTTY, &ghostty("JetBrains Darcula", "22", "epoll"));
    let run = sb.rig(&["diff", GHOSTTY_TARGET]);
    assert_eq!(run.status, 1);
    assert!(run.stdout.contains("--- repo"), "{}", run.stdout);
    assert!(run.stdout.contains("+++ disk"), "{}", run.stdout);
    assert!(run.stdout.contains("-font-size = 20"), "{}", run.stdout);
    assert!(run.stdout.contains("+font-size = 22"), "{}", run.stdout);
}

#[test]
fn diff_refuses_an_unmanaged_path() {
    let sb = home_only();
    up(&sb, &[]);
    let run = sb.rig(&["diff", "~/nope"]);
    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("not managed"), "{}", run.stderr);
}

#[test]
fn dry_run_changes_nothing() {
    let sb = home_only();
    let run = up(&sb, &["-n"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(run.rows().iter().all(|(o, _)| o == "created"));
    assert!(!sb.state_dir().exists());
    assert!(!sb.home_exists(GHOSTTY));
}

#[test]
fn orphan_flow() {
    let sb = home_only();
    up(&sb, &[]);
    std::fs::remove_file(sb.repo.join(GHOSTTY_REPO)).unwrap();
    let run = sb.rig(&["up"]);
    assert_eq!(run.outcome(GHOSTTY_TARGET).unwrap(), "orphaned");
    assert_eq!(run.status, 1);
    assert!(sb.home_exists(GHOSTTY));

    let pruned = sb.rig(&["up", "--prune"]);
    assert_eq!(pruned.outcome(GHOSTTY_TARGET).unwrap(), "orphaned");
    assert!(!sb.home_exists(GHOSTTY));
}

#[test]
fn orphan_edited_is_kept_by_prune() {
    let sb = home_only();
    up(&sb, &[]);
    std::fs::remove_file(sb.repo.join(GHOSTTY_REPO)).unwrap();
    sb.write_home(GHOSTTY, "mine\n");
    let run = sb.rig(&["up", "--prune"]);
    assert_eq!(run.outcome(GHOSTTY_TARGET).unwrap(), "orphaned");
    assert!(sb.home_exists(GHOSTTY));
}

#[test]
fn state_dir_deleted_recovers() {
    let sb = home_only();
    up(&sb, &[]);
    std::fs::remove_dir_all(sb.state_dir()).unwrap();
    let run = sb.rig(&["up"]);
    assert!(
        run.rows().iter().all(|(o, _)| o == "adopted"),
        "{}",
        run.stdout
    );
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
}

#[test]
fn host_from_config_file() {
    let sb = home_only();
    std::fs::create_dir_all(sb.root.path().join("config/rig")).unwrap();
    std::fs::write(sb.root.path().join("config/rig/host"), "desktop\n").unwrap();
    let run = sb.rig(&["up", sb.repo.to_str().unwrap()]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(sb.home_exists(GHOSTTY));
}

#[test]
fn host_unknown_errors() {
    let sb = home_only();
    let run = sb.rig(&["up", "--host", "laptop", sb.repo.to_str().unwrap()]);
    assert_eq!(run.status, 2);
    assert!(
        run.stderr.contains("unknown host 'laptop'"),
        "{}",
        run.stderr
    );
}

#[test]
fn repo_path_remembered() {
    let sb = home_only();
    up(&sb, &[]);
    let remembered = std::fs::read_to_string(sb.root.path().join("config/rig/repo")).unwrap();
    assert_eq!(
        remembered.trim(),
        sb.repo.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(sb.rig(&["status"]).status, 0);
}

#[test]
fn no_repo_configured_is_an_error() {
    let sb = Sandbox::new();
    let run = sb.rig(&["up"]);
    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("no repo"), "{}", run.stderr);
}

// /etc targets.

const NVIDIA: &str = "etc/modprobe.d/nvidia.conf";

fn etc_sandbox(sudo: &str) -> Sandbox {
    let sb = Sandbox::with_fixture("basic").with_os("linux:arch");
    sb.fake_bin("sudo", sudo);
    sb
}

fn etc_up(sb: &Sandbox, extra: &[&str]) -> common::Run {
    let repo = sb.repo.to_str().unwrap().to_string();
    let etc_root = sb.etc_root().to_str().unwrap().to_string();
    let mut args = vec!["up", "--host", "desktop", "--etc-root", &etc_root, &repo];
    args.extend_from_slice(extra);
    sb.rig(&args)
}

#[test]
fn etc_escalates_through_sudo() {
    let sb = etc_sandbox(FAKE_SUDO);
    let run = etc_up(&sb, &[]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stderr.contains("escalating: 1 /etc targets need root"),
        "{}",
        run.stderr
    );

    let expected = format!(
        "sudo -n -- true\nsudo -n -- {} up --etc-only --etc-root {} --host desktop --repo {}",
        env!("CARGO_BIN_EXE_rig"),
        sb.etc_root().display(),
        sb.repo.canonicalize().unwrap().display()
    );
    assert_eq!(sb.log().join("\n"), expected);

    let written = std::fs::read_to_string(sb.etc_root().join(NVIDIA)).unwrap();
    assert_eq!(written, "options nvidia_drm modeset=1\n");
    assert!(sb.etc_root().join("var/lib/rig/manifest.toml").exists());
    assert!(
        run.stdout.contains("/etc/modprobe.d/nvidia.conf"),
        "{}",
        run.stdout
    );
    // The home phase ran too.
    assert!(sb.home_exists(GHOSTTY));
}

#[test]
fn etc_unchanged_does_not_escalate() {
    let sb = etc_sandbox(FAKE_SUDO);
    etc_up(&sb, &[]);
    std::fs::write(sb.log_path(), "").unwrap();
    let run = etc_up(&sb, &[]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(sb.log().is_empty(), "{:?}", sb.log());
}

#[test]
fn etc_no_sudo_flag_prints_needs_root_row() {
    let sb = etc_sandbox(FAKE_SUDO);
    let run = etc_up(&sb, &["--no-sudo"]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("needs-root"), "{}", run.stdout);
    assert!(sb.log().is_empty(), "{:?}", sb.log());
    assert!(sb.home_exists(GHOSTTY));
    assert!(!sb.etc_root().join(NVIDIA).exists());
}

#[test]
fn etc_sudo_n_failure_row() {
    let sb = etc_sandbox(FAKE_SUDO_DENIES_N);
    let run = etc_up(&sb, &[]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("needs-root"), "{}", run.stdout);
    assert!(sb.home_exists(GHOSTTY));
}

#[test]
fn etc_edited_file_reports_without_root() {
    let sb = etc_sandbox(FAKE_SUDO);
    etc_up(&sb, &[]);
    std::fs::write(sb.etc_root().join(NVIDIA), "options nvidia_drm modeset=0\n").unwrap();
    std::fs::write(sb.log_path(), "").unwrap();
    let run = etc_up(&sb, &[]);
    assert_eq!(
        run.outcome("/etc/modprobe.d/nvidia.conf").unwrap(),
        "edited"
    );
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(sb.log().is_empty(), "{:?}", sb.log());
}

#[test]
fn etc_dry_run_does_not_escalate() {
    let sb = etc_sandbox(FAKE_SUDO);
    let run = etc_up(&sb, &["-n"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(sb.log().is_empty(), "{:?}", sb.log());
    assert!(run.stdout.contains("would write"), "{}", run.stdout);
    assert!(!sb.etc_root().join(NVIDIA).exists());
}
