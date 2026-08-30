mod common;

use common::Sandbox;

fn sandbox() -> Sandbox {
    Sandbox::with_fixture("basic").with_os("linux:ubuntu")
}

fn up(sb: &Sandbox) -> common::Run {
    sb.rig(&["up", "--host", "desktop", sb.repo.to_str().unwrap()])
}

#[test]
fn adopt_copies_file_and_up_is_noop() {
    let sb = sandbox();
    up(&sb);
    sb.write_home(".config/foo/bar", "hello\n");
    let run = sb.rig(&["adopt", "~/.config/foo/bar", "--module", "core"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("-> modules/core/home/.config/foo/bar"),
        "{}",
        run.stdout
    );
    assert_eq!(
        std::fs::read_to_string(sb.repo.join("modules/core/home/.config/foo/bar")).unwrap(),
        "hello\n"
    );
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
}

#[test]
fn adopt_host_variant() {
    let sb = sandbox();
    up(&sb);
    sb.write_home(".config/foo/bar", "hello\n");
    let run = sb.rig(&[
        "adopt",
        "~/.config/foo/bar",
        "--module",
        "core",
        "--host-variant",
    ]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(sb
        .repo
        .join("modules/core/home/.config/foo/bar@desktop")
        .exists());
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
}

#[test]
fn adopt_keeps_the_mode() {
    let sb = sandbox();
    up(&sb);
    sb.write_home(".local/bin/mine", "#!/bin/sh\n");
    std::fs::set_permissions(
        sb.home.join(".local/bin/mine"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    sb.rig(&["adopt", "~/.local/bin/mine", "--module", "core"]);
    let mode = std::os::unix::fs::MetadataExt::mode(
        &std::fs::metadata(sb.repo.join("modules/core/home/.local/bin/mine")).unwrap(),
    ) & 0o777;
    assert_eq!(mode, 0o755);
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
}

#[test]
fn adopt_refuses_managed() {
    let sb = sandbox();
    up(&sb);
    let run = sb.rig(&["adopt", "~/.config/ghostty/config", "--module", "core"]);
    assert_eq!(run.status, 2);
    assert!(
        run.stderr.contains("already managed by ghostty"),
        "{}",
        run.stderr
    );
}

#[test]
fn adopt_refuses_inactive_module() {
    let sb = sandbox().with_os("macos");
    sb.rig(&["up", "--host", "macbook", sb.repo.to_str().unwrap()]);
    sb.write_home(".config/foo/bar", "hello\n");
    let run = sb.rig(&["adopt", "~/.config/foo/bar", "--module", "hypr"]);
    assert_eq!(run.status, 2);
    assert!(
        run.stderr.contains("not active for host macbook"),
        "{}",
        run.stderr
    );
}

#[test]
fn adopt_refuses_unknown_module() {
    let sb = sandbox();
    up(&sb);
    sb.write_home(".config/foo/bar", "hello\n");
    let run = sb.rig(&["adopt", "~/.config/foo/bar", "--module", "nope"]);
    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("no such module nope"), "{}", run.stderr);
}

#[test]
fn adopt_refuses_a_directory() {
    let sb = sandbox();
    up(&sb);
    let run = sb.rig(&["adopt", "~/.config/foo", "--module", "core"]);
    assert_eq!(run.status, 2);
}
