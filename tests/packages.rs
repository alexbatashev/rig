mod common;

use common::Sandbox;

const FAKE: &str = r#"#!/bin/sh
name=$(basename "$0")
printf '%s %s\n' "$name" "$*" >> "$FAKE_LOG"
case "$name $1" in
  "sudo "*)
    shift
    [ "$1" = "-n" ] && shift
    [ "$1" = "--" ] && shift
    exec "$@" ;;
  "pacman -Qq") cat "$FAKE_INSTALLED" ;;
  "pacman -Sp")
    shift 3
    rc=0
    for p in "$@"; do
      grep -qx "$p" "$FAKE_REPO" || { echo "error: target not found: $p" >&2; rc=1; }
    done
    exit $rc ;;
  "pacman -S")
    shift 3
    for p in "$@"; do echo "$p" >> "$FAKE_INSTALLED"; done ;;
  "pacman -Rns")
    shift 2
    for p in "$@"; do grep -vx "$p" "$FAKE_INSTALLED" > "$FAKE_INSTALLED.t"; mv "$FAKE_INSTALLED.t" "$FAKE_INSTALLED"; done ;;
  "yay -S"|"paru -S")
    shift 3
    for p in "$@"; do echo "$p" >> "$FAKE_INSTALLED"; done ;;
  "dpkg-query -W") sed 's/$/\tinstalled/' "$FAKE_INSTALLED" ;;
  "apt-get install")
    shift 3
    for p in "$@"; do echo "$p" >> "$FAKE_INSTALLED"; done ;;
  "apt-get remove")
    shift 2
    for p in "$@"; do grep -vx "$p" "$FAKE_INSTALLED" > "$FAKE_INSTALLED.t"; mv "$FAKE_INSTALLED.t" "$FAKE_INSTALLED"; done ;;
  "apt-get -v") echo "apt 2.7.0" ;;
  "brew list") cat "$FAKE_INSTALLED" ;;
  "brew install")
    shift
    if [ "$1" = "--cask" ]; then shift; echo "$1" >> "$FAKE_INSTALLED"; exit 0; fi
    if grep -qx "$1" "$FAKE_CASKS" 2>/dev/null; then echo "No available formula with the name \"$1\"" >&2; exit 1; fi
    echo "$1" >> "$FAKE_INSTALLED" ;;
  "brew uninstall")
    shift
    [ "$1" = "--cask" ] && shift
    grep -vx "$1" "$FAKE_INSTALLED" > "$FAKE_INSTALLED.t"; mv "$FAKE_INSTALLED.t" "$FAKE_INSTALLED" ;;
  "brew --version") echo "Homebrew 4.0.0" ;;
  "nix profile")
    case "$2" in
      list) cat "$FAKE_NIX_JSON" ;;
      install) shift 2; for p in "$@"; do echo "$p" >> "$FAKE_LOG.nix"; done ;;
      remove) [ -n "$FAKE_NIX_UNKNOWN" ] && { echo "error: unknown element" >&2; exit 1; } ;;
    esac ;;
  "nix --version") echo "nix (Nix) 2.24.9" ;;
  "pacman -V") echo "Pacman v6.1.0" ;;
esac
exit 0
"#;

fn sandbox(os: &str) -> Sandbox {
    let sb = Sandbox::with_fixture("pkgs").with_os(os);
    for tool in [
        "sudo",
        "pacman",
        "yay",
        "paru",
        "apt-get",
        "dpkg-query",
        "brew",
        "nix",
    ] {
        sb.fake_bin(tool, FAKE);
    }
    sb.set_installed(&[]);
    sb.set_repo_packages(&["ghostty", "ripgrep", "vim"]);
    sb.set_nix_json("{\"elements\":{}}");
    sb
}

fn up(sb: &Sandbox, extra: &[&str]) -> common::Run {
    let repo = sb.repo.to_str().unwrap().to_string();
    let mut args = vec!["up", "--host", "box", "-y", &repo];
    args.extend_from_slice(extra);
    sb.rig(&args)
}

fn manifest(sb: &Sandbox) -> String {
    std::fs::read_to_string(sb.state_dir().join("manifest.toml")).unwrap()
}

mod arch {
    use super::{manifest, sandbox, up};

    #[test]
    fn installs_missing() {
        let sb = sandbox("linux:arch");
        let run = up(&sb, &[]);
        assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
        assert!(
            sb.log()
                .contains(&"pacman -S --needed --noconfirm ghostty ripgrep".to_string()),
            "{:?}",
            sb.log()
        );
        let m = manifest(&sb);
        assert!(m.contains("arch = [\"ghostty\", \"ripgrep\"]"), "{m}");
        assert!(run.stdout.contains("package"), "{}", run.stdout);
    }

    #[test]
    fn second_up_no_install() {
        let sb = sandbox("linux:arch");
        up(&sb, &[]);
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        assert!(
            sb.log().iter().all(|l| !l.starts_with("pacman -S ")),
            "{:?}",
            sb.log()
        );
    }

    #[test]
    fn delist_removes_only_tracked() {
        let sb = sandbox("linux:arch");
        sb.set_installed(&["vim"]);
        sb.write_repo(
            "modules/tools/module.toml",
            "[packages]\narch = [\"ghostty\", \"vim\"]\n",
        );
        up(&sb, &[]);
        sb.write_repo("modules/tools/module.toml", "[packages]\narch = []\n");
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        let log = sb.log().join("\n");
        assert!(log.contains("pacman -Rns --noconfirm ghostty"), "{log}");
        assert!(!log.contains("vim"), "{log}");
    }

    #[test]
    fn aur_fallback_uses_yay() {
        let sb = sandbox("linux:arch");
        sb.set_repo_packages(&["ripgrep"]);
        up(&sb, &[]);
        let log = sb.log().join("\n");
        assert!(log.contains("yay -S --needed --noconfirm ghostty"), "{log}");
        assert!(
            log.contains("pacman -S --needed --noconfirm ripgrep"),
            "{log}"
        );
    }

    #[test]
    fn paru_when_no_yay() {
        let sb = sandbox("linux:arch");
        sb.remove_fake_bin("yay");
        sb.set_repo_packages(&["ripgrep"]);
        up(&sb, &[]);
        assert!(sb.log().join("\n").contains("paru -S"), "{:?}", sb.log());
    }

    #[test]
    fn no_aur_helper_errors() {
        let sb = sandbox("linux:arch");
        sb.remove_fake_bin("yay");
        sb.remove_fake_bin("paru");
        sb.set_repo_packages(&["ripgrep"]);
        let run = up(&sb, &[]);
        assert_eq!(run.status, 2, "{}{}", run.stdout, run.stderr);
        assert!(run.stdout.contains("error"), "{}", run.stdout);
    }

    #[test]
    fn install_failure_keeps_tracked_clean() {
        let sb = sandbox("linux:arch");
        sb.fake_bin("pacman", "#!/bin/sh\nprintf 'pacman %s\\n' \"$*\" >> \"$FAKE_LOG\"\n[ \"$1\" = \"-Qq\" ] && exit 0\n[ \"$1\" = \"-Sp\" ] && exit 0\necho boom >&2\nexit 1\n");
        let run = up(&sb, &[]);
        assert_eq!(run.status, 2, "{}{}", run.stdout, run.stderr);
        assert!(!manifest(&sb).contains("ghostty"), "{}", manifest(&sb));
    }
}

mod apt {
    use super::{manifest, sandbox, up};

    #[test]
    fn second_up_no_install() {
        let sb = sandbox("linux:ubuntu");
        up(&sb, &[]);
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        assert!(
            !sb.log().iter().any(|l| l.starts_with("apt-get install")),
            "{:?}",
            sb.log()
        );
    }

    #[test]
    fn installs_missing() {
        let sb = sandbox("linux:ubuntu");
        up(&sb, &[]);
        assert!(
            sb.log()
                .contains(&"apt-get install -y --no-install-recommends ghostty".to_string()),
            "{:?}",
            sb.log()
        );
        assert!(manifest(&sb).contains("ubuntu = [\"ghostty\"]"));
    }

    #[test]
    fn delist_removes_only_tracked() {
        let sb = sandbox("linux:ubuntu");
        sb.set_installed(&["vim"]);
        sb.write_repo(
            "modules/tools/module.toml",
            "[packages]\nubuntu = [\"ghostty\", \"vim\"]\n",
        );
        up(&sb, &[]);
        sb.write_repo("modules/tools/module.toml", "[packages]\nubuntu = []\n");
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        let log = sb.log().join("\n");
        assert!(log.contains("apt-get remove -y ghostty"), "{log}");
        assert!(!log.contains("vim"), "{log}");
    }
}

mod brew {
    use super::{sandbox, up};

    #[test]
    fn plain_formula_install_and_second_up_no_install() {
        let sb = sandbox("macos");
        up(&sb, &[]);
        assert!(
            sb.log().contains(&"brew install ghostty".to_string()),
            "{:?}",
            sb.log()
        );
        assert!(
            !sb.log()
                .iter()
                .any(|l| l.starts_with("brew install") && l.contains("--cask")),
            "{:?}",
            sb.log()
        );
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        assert!(
            !sb.log().iter().any(|l| l.starts_with("brew install")),
            "{:?}",
            sb.log()
        );
    }

    #[test]
    fn delist_removes_only_tracked() {
        let sb = sandbox("macos");
        sb.set_installed(&["vim"]);
        sb.write_repo(
            "modules/tools/module.toml",
            "[packages]\nmacos = [\"ghostty\", \"vim\"]\n",
        );
        up(&sb, &[]);
        sb.write_repo("modules/tools/module.toml", "[packages]\nmacos = []\n");
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        let log = sb.log().join("\n");
        assert!(log.contains("brew uninstall ghostty"), "{log}");
        assert!(!log.contains("vim"), "{log}");
    }

    #[test]
    fn cask_fallback_and_remove_uses_cask_flag() {
        let sb = sandbox("macos");
        sb.set_casks(&["ghostty"]);
        up(&sb, &[]);
        let log = sb.log().join("\n");
        assert!(log.contains("brew install ghostty"), "{log}");
        assert!(log.contains("brew install --cask ghostty"), "{log}");
        let m = std::fs::read_to_string(sb.state_dir().join("manifest.toml")).unwrap();
        assert!(m.contains("macos_casks = [\"ghostty\"]"), "{m}");

        sb.write_repo("modules/tools/module.toml", "[packages]\nmacos = []\n");
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        assert!(
            sb.log()
                .join("\n")
                .contains("brew uninstall --cask ghostty"),
            "{:?}",
            sb.log()
        );
    }
}

mod nix {
    use super::{sandbox, up};

    #[test]
    fn second_up_no_install() {
        let sb = sandbox("linux:arch");
        up(&sb, &[]);
        sb.set_nix_json("{\"elements\":{\"bloaty\":{}}}");
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        assert!(
            !sb.log()
                .iter()
                .any(|l| l.starts_with("nix profile install")),
            "{:?}",
            sb.log()
        );
    }

    #[test]
    fn old_list_format_elements_are_recognised() {
        let sb = sandbox("linux:arch");
        sb.set_nix_json(
            "{\"elements\":[{\"originalUrl\":\"flake:nixpkgs\",\"attrPath\":\"nixpkgs#bloaty\"}]}",
        );
        up(&sb, &[]);
        assert!(
            !sb.log()
                .iter()
                .any(|l| l.starts_with("nix profile install")),
            "{:?}",
            sb.log()
        );
    }

    #[test]
    fn delist_removes_only_tracked() {
        let sb = sandbox("linux:arch");
        up(&sb, &[]);
        sb.set_nix_json("{\"elements\":{\"bloaty\":{}}}");
        sb.write_repo("modules/tools/module.toml", "[packages]\nnix = []\n");
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        assert!(
            sb.log().iter().any(|l| l.starts_with("nix profile remove")),
            "{:?}",
            sb.log()
        );
    }

    #[test]
    fn name_from_flake_ref() {
        let sb = sandbox("linux:arch");
        sb.set_nix_json("{\"elements\":{\"bloaty\":{}}}");
        up(&sb, &[]);
        assert!(
            sb.log()
                .iter()
                .all(|l| !l.starts_with("nix profile install")),
            "{:?}",
            sb.log()
        );

        sb.set_nix_json("{\"elements\":{}}");
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        assert!(
            sb.log()
                .contains(&"nix profile install nixpkgs#bloaty".to_string()),
            "{:?}",
            sb.log()
        );
    }

    #[test]
    fn remove_regex_fallback() {
        let mut sb = sandbox("linux:arch");
        up(&sb, &[]);
        sb.set_nix_json("{\"elements\":{\"bloaty\":{}}}");
        sb.write_repo("modules/tools/module.toml", "[packages]\nnix = []\n");
        sb.set_env("FAKE_NIX_UNKNOWN", "1");
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        assert!(
            sb.log()
                .contains(&"nix profile remove --regex ^bloaty$".to_string()),
            "{:?}",
            sb.log()
        );
    }
}

#[test]
fn declined_without_yes_exits_1() {
    let sb = sandbox("linux:arch");
    let run = sb.rig(&["up", "--host", "box", sb.repo.to_str().unwrap()]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("declined"), "{}", run.stdout);
    assert!(
        sb.log().iter().all(|l| !l.starts_with("pacman -S ")),
        "{:?}",
        sb.log()
    );
}

#[test]
fn unavailable_backend_row() {
    let sb = sandbox("linux:arch");
    sb.remove_fake_bin("nix");
    let run = up(&sb, &[]);
    assert!(run.stdout.contains("packages: nix"), "{}", run.stdout);
    assert!(run.stdout.contains("not on PATH"), "{}", run.stdout);
}

#[test]
fn mise_is_reported_as_unavailable() {
    let sb = sandbox("linux:arch");
    sb.write_repo(
        "modules/tools/module.toml",
        "[packages]\nmise = [\"npm:@anthropic-ai/claude-code\"]\n",
    );
    let run = up(&sb, &[]);
    assert!(run.stdout.contains("packages: mise"), "{}", run.stdout);
    assert!(
        run.stdout.contains("backend not available"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_hand_removed_tracked_package_does_not_wedge_the_run() {
    let sb = sandbox("linux:arch");
    up(&sb, &[]);
    // The user removes ghostty by hand, then it leaves the repo.
    sb.set_installed(&["ripgrep"]);
    sb.write_repo("modules/tools/module.toml", "[packages]\narch = []\n");
    let run = up(&sb, &[]);
    assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
    let log = sb.log().join("\n");
    assert!(!log.contains("-Rns --noconfirm ghostty"), "{log}");
    assert!(log.contains("pacman -Rns --noconfirm ripgrep"), "{log}");

    // The ledger is clean, so the next run has nothing left to do.
    std::fs::write(sb.log_path(), "").unwrap();
    let again = up(&sb, &[]);
    assert_eq!(again.status, 0, "{}{}", again.stdout, again.stderr);
    assert!(
        !sb.log().iter().any(|l| l.contains("-Rns")),
        "{:?}",
        sb.log()
    );
}

#[test]
fn dry_run_queries_only() {
    let sb = sandbox("linux:arch");
    let run = up(&sb, &["-n"]);
    assert!(run.stdout.contains("would install"), "{}", run.stdout);
    assert!(
        sb.log().iter().all(|l| !l.starts_with("pacman -S ")),
        "{:?}",
        sb.log()
    );
}

#[test]
fn keel_stub_errors() {
    let sb = sandbox("linux:keel");
    sb.write_repo(
        "modules/tools/module.toml",
        "[packages]\nkeel = [\"ghostty\"]\n",
    );
    let run = up(&sb, &[]);
    assert_eq!(run.status, 2, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("not implemented yet"), "{}", run.stdout);
}

#[test]
fn doctor_lists_backends() {
    let sb = sandbox("linux:arch");
    let run = sb.rig(&["doctor", sb.repo.to_str().unwrap()]);
    assert!(run.stdout.contains("Pacman v6.1.0"), "{}", run.stdout);
    assert!(run.stdout.contains("nix (Nix)"), "{}", run.stdout);
}
