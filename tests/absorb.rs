mod common;

use common::Sandbox;

const GHOSTTY: &str = ".config/ghostty/config";
const GHOSTTY_TARGET: &str = "~/.config/ghostty/config";
const GHOSTTY_REPO: &str = "modules/ghostty/home/.config/ghostty/config";

fn sandbox() -> Sandbox {
    Sandbox::with_fixture("basic").with_os("linux:ubuntu")
}

fn up(sb: &Sandbox) -> common::Run {
    sb.rig(&["up", "--host", "desktop", sb.repo.to_str().unwrap()])
}

fn ghostty(size: &str) -> String {
    format!(
        "theme = JetBrains Darcula\nfont-size = {size}\nshell-integration-features = true\nkeybind = global:cmd+backquote=toggle_quick_terminal\nasync-backend = epoll\n"
    )
}

#[test]
fn daily_loop() {
    let sb = sandbox();
    up(&sb);
    sb.write_home(GHOSTTY, &ghostty("22"));

    let status = sb.rig(&["status"]);
    assert_eq!(status.outcome(GHOSTTY_TARGET).unwrap(), "edited");

    let run = sb.rig(&["absorb", GHOSTTY_TARGET]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        run.stdout.trim_end(),
        "  ghostty: home/.config/ghostty/config  font-size: 20 -> 22"
    );
    assert_eq!(
        std::fs::read_to_string(sb.repo.join(GHOSTTY_REPO)).unwrap(),
        ghostty("22")
    );
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
}

#[test]
fn daily_loop_into_host_variant() {
    let sb = sandbox();
    sb.write_repo(
        "modules/ghostty/home/.config/ghostty/config@desktop",
        "font-size = 21\n",
    );
    up(&sb);
    let base_before = std::fs::read_to_string(sb.repo.join(GHOSTTY_REPO)).unwrap();

    sb.write_home(GHOSTTY, &ghostty("23"));
    let run = sb.rig(&["absorb", GHOSTTY_TARGET]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("config@desktop"), "{}", run.stdout);
    assert_eq!(
        std::fs::read_to_string(sb.repo.join(GHOSTTY_REPO)).unwrap(),
        base_before
    );
    assert_eq!(
        std::fs::read_to_string(
            sb.repo
                .join("modules/ghostty/home/.config/ghostty/config@desktop")
        )
        .unwrap(),
        "font-size = 23\n"
    );
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
}

#[test]
fn daily_loop_text_file() {
    let sb = sandbox();
    up(&sb);
    let repo_file = "modules/hypr/home/.config/hypr/bindings.lua";
    let before = std::fs::read_to_string(sb.repo.join(repo_file)).unwrap();
    let edited = before.replace("exec, ghostty", "exec, alacritty");
    sb.write_home(".config/hypr/bindings.lua", &edited);

    let run = sb.rig(&["absorb", "~/.config/hypr/bindings.lua"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        std::fs::read_to_string(sb.repo.join(repo_file)).unwrap(),
        edited
    );
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
}

#[test]
fn absorb_refuses_when_repo_changed_too() {
    let sb = sandbox();
    up(&sb);
    sb.write_home(GHOSTTY, &ghostty("22"));
    sb.write_repo(
        GHOSTTY_REPO,
        &ghostty("20").replace("async-backend = epoll", "async-backend = io_uring"),
    );

    let refused = sb.rig(&["absorb", GHOSTTY_TARGET]);
    assert_eq!(refused.status, 2, "{}{}", refused.stdout, refused.stderr);
    assert!(
        refused.stderr.contains("repo changed too"),
        "{}",
        refused.stderr
    );

    assert_eq!(sb.rig(&["up"]).outcome(GHOSTTY_TARGET).unwrap(), "merged");
    let run = sb.rig(&["absorb", GHOSTTY_TARGET]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
    assert_eq!(
        sb.read_home(GHOSTTY),
        ghostty("22").replace("async-backend = epoll", "async-backend = io_uring")
    );
}

#[test]
fn absorb_refuses_an_unchanged_file() {
    let sb = sandbox();
    up(&sb);
    let run = sb.rig(&["absorb", GHOSTTY_TARGET]);
    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("nothing to absorb"), "{}", run.stderr);
}

#[test]
fn absorb_refuses_an_unmanaged_file() {
    let sb = sandbox();
    up(&sb);
    sb.write_home(".config/other/thing", "x\n");
    let run = sb.rig(&["absorb", "~/.config/other/thing"]);
    assert_eq!(run.status, 2);
    assert!(run.stderr.contains("not managed"), "{}", run.stderr);
}

#[test]
fn absorb_all_skips_manual_modules() {
    let sb = sandbox();
    sb.write_repo(
        "modules/ghostty/module.toml",
        "sync = \"manual\"\n\n[packages]\narch = [\"ghostty\"]\n",
    );
    up(&sb);
    sb.write_home(GHOSTTY, &ghostty("22"));
    let hypr = "modules/hypr/home/.config/hypr/bindings.lua";
    let edited = std::fs::read_to_string(sb.repo.join(hypr))
        .unwrap()
        .replace("exec, ghostty", "exec, alacritty");
    sb.write_home(".config/hypr/bindings.lua", &edited);

    let run = sb.rig(&["absorb", "--all"]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("skipped"), "{}", run.stdout);
    assert!(run.stdout.contains("sync = manual"), "{}", run.stdout);
    // The auto module still absorbed.
    assert_eq!(std::fs::read_to_string(sb.repo.join(hypr)).unwrap(), edited);
    assert_eq!(
        std::fs::read_to_string(sb.repo.join(GHOSTTY_REPO)).unwrap(),
        ghostty("20")
    );
}

#[test]
fn absorb_normalizes_formatting() {
    let sb = sandbox();
    up(&sb);
    sb.write_home(
        GHOSTTY,
        &ghostty("22").replace("font-size = 22", "font-size=22"),
    );
    let run = sb.rig(&["absorb", GHOSTTY_TARGET]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("normalized"), "{}", run.stdout);
    assert_eq!(sb.read_home(GHOSTTY), ghostty("22"));
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
}

#[test]
fn absorb_toml_touches_one_key() {
    let sb = sandbox();
    up(&sb);
    let before = sb.read_home(".config/starship.toml");
    sb.write_home(
        ".config/starship.toml",
        &before.replace("truncation_length = 5", "truncation_length = 8"),
    );
    let run = sb.rig(&["absorb", "~/.config/starship.toml"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("directory.truncation_length: 5 -> 8"),
        "{}",
        run.stdout
    );
    // The change lands in the @desktop variant, which is the top layer for this host.
    let variant = std::fs::read_to_string(
        sb.repo
            .join("modules/core/home/.config/starship.toml@desktop"),
    )
    .unwrap();
    assert!(variant.contains("# the desktop has room"), "{variant}");
    assert!(variant.contains("truncation_length = 8"), "{variant}");
    let base =
        std::fs::read_to_string(sb.repo.join("modules/core/home/.config/starship.toml")).unwrap();
    assert!(base.contains("truncation_length = 3"), "{base}");
}

#[test]
fn daily_loop_from_init() {
    let sb = Sandbox::new().with_os("linux:ubuntu");
    let repo = sb.root.path().join("dotfiles");
    let init = sb.rig(&["init", repo.to_str().unwrap(), "--host", "desktop"]);
    assert_eq!(init.status, 0, "{}{}", init.stdout, init.stderr);

    std::fs::create_dir_all(repo.join("modules/ghostty/home/.config/ghostty")).unwrap();
    std::fs::write(
        repo.join("modules/ghostty/home/.config/ghostty/config"),
        ghostty("20"),
    )
    .unwrap();
    std::fs::write(repo.join("hosts/desktop.toml"), "modules = [\"ghostty\"]\n").unwrap();

    let up = sb.rig(&["up"]);
    assert_eq!(
        up.outcome(GHOSTTY_TARGET).unwrap(),
        "created",
        "{}",
        up.stdout
    );

    sb.write_home(GHOSTTY, &ghostty("22"));
    assert_eq!(
        sb.rig(&["status"]).outcome(GHOSTTY_TARGET).unwrap(),
        "edited"
    );

    let run = sb.rig(&["absorb", GHOSTTY_TARGET]);
    assert_eq!(
        run.stdout.trim_end(),
        "  ghostty: home/.config/ghostty/config  font-size: 20 -> 22"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("modules/ghostty/home/.config/ghostty/config")).unwrap(),
        ghostty("22")
    );
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
}

#[test]
fn absorb_an_etc_target() {
    let sb = Sandbox::with_fixture("basic").with_os("linux:arch");
    sb.fake_bin(
        "sudo",
        "#!/bin/sh\n[ \"$1\" = \"-n\" ] && shift\n[ \"$1\" = \"--\" ] && shift\nexec \"$@\"\n",
    );
    let etc_root = sb.etc_root().to_str().unwrap().to_string();
    let repo = sb.repo.to_str().unwrap();
    sb.rig(&["up", "--host", "desktop", "--etc-root", &etc_root, repo]);

    let conf = sb.etc_root().join("etc/modprobe.d/nvidia.conf");
    std::fs::write(&conf, "options nvidia_drm modeset=0\n").unwrap();
    let status = sb.rig(&["status", "--etc-root", &etc_root]);
    assert_eq!(
        status.outcome("/etc/modprobe.d/nvidia.conf").unwrap(),
        "edited",
        "{}",
        status.stdout
    );

    let run = sb.rig(&[
        "absorb",
        "--etc-root",
        &etc_root,
        "/etc/modprobe.d/nvidia.conf",
    ]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert_eq!(
        std::fs::read_to_string(sb.repo.join("modules/nvidia/etc/modprobe.d/nvidia.conf")).unwrap(),
        "options nvidia_drm modeset=0\n"
    );
    let after = sb.rig(&["up", "--host", "desktop", "--etc-root", &etc_root]);
    assert_eq!(after.stdout, "nothing to do\n", "{}", after.stdout);
}

#[test]
fn verbose_lists_every_change_when_there_are_many() {
    let many = |sb: &Sandbox| {
        let before = sb.read_home(".config/starship.toml");
        let mut edited = before.replace("truncation_length = 5", "truncation_length = 8");
        edited.push_str("\n[git_status]\na = \"1\"\nb = \"2\"\nc = \"3\"\nd = \"4\"\n");
        sb.write_home(".config/starship.toml", &edited);
    };

    let quiet_sb = sandbox();
    up(&quiet_sb);
    many(&quiet_sb);
    let quiet = quiet_sb.rig(&["absorb", "~/.config/starship.toml"]);
    assert_eq!(quiet.status, 0, "{}{}", quiet.stdout, quiet.stderr);
    assert!(quiet.stdout.contains("5 changes"), "{}", quiet.stdout);
    assert_eq!(quiet.stdout.lines().count(), 1, "{}", quiet.stdout);

    let loud_sb = sandbox();
    up(&loud_sb);
    many(&loud_sb);
    let loud = loud_sb.rig(&["absorb", "-v", "~/.config/starship.toml"]);
    assert_eq!(loud.status, 0, "{}{}", loud.stdout, loud.stderr);
    assert!(
        loud.stdout.contains("git_status.a: added"),
        "{}",
        loud.stdout
    );
    assert!(loud.stdout.lines().count() > 5, "{}", loud.stdout);
}
