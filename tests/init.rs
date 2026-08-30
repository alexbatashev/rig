mod common;

use common::Sandbox;

#[test]
fn init_creates_skeleton_and_registers_host() {
    let sb = Sandbox::new();
    let dir = sb.root.path().join("dotfiles");
    let run = sb.rig(&["init", dir.to_str().unwrap(), "--host", "desktop"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    for rel in [
        "rig.toml",
        "hosts/desktop.toml",
        "modules/.keep",
        ".gitignore",
    ] {
        assert!(dir.join(rel).exists(), "missing {rel}");
    }
    assert!(dir.join(".git").exists());
    assert_eq!(
        std::fs::read_to_string(sb.root.path().join("config/rig/host"))
            .unwrap()
            .trim(),
        "desktop"
    );
    assert_eq!(
        std::fs::read_to_string(sb.root.path().join("config/rig/repo"))
            .unwrap()
            .trim(),
        dir.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(sb.rig(&["up"]).stdout, "nothing to do\n");
}

#[test]
fn init_refuses_to_overwrite() {
    let sb = Sandbox::new();
    let dir = sb.root.path().join("dotfiles");
    std::fs::create_dir_all(dir.join("hosts")).unwrap();
    std::fs::write(dir.join("rig.toml"), "defaults = []\n").unwrap();
    std::fs::write(dir.join("hosts/desktop.toml"), "modules = [\"keep\"]\n").unwrap();
    sb.rig(&["init", dir.to_str().unwrap(), "--host", "desktop"]);
    assert_eq!(
        std::fs::read_to_string(dir.join("rig.toml")).unwrap(),
        "defaults = []\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("hosts/desktop.toml")).unwrap(),
        "modules = [\"keep\"]\n"
    );
}

#[test]
fn init_existing_repo_registers_host_only() {
    let sb = Sandbox::with_fixture("basic");
    let run = sb.rig(&["init", sb.repo.to_str().unwrap(), "--host", "laptop"]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    assert!(sb.repo.join("hosts/laptop.toml").exists());
    assert!(!sb.repo.join("modules/.keep").exists());
}
