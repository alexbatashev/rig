mod common;

use common::Sandbox;

const FAKE: &str = r#"#!/bin/sh
name=$(basename "$0")
printf '%s %s\n' "$name" "$*" >> "$FAKE_LOG"
missing() { grep -qx "$1" "$FAKE_MISSING" 2>/dev/null; }
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
    for p in "$@"; do
      missing "$p" && { echo " -> Could not find all required packages: $p" >&2; exit 1; }
      echo "$p" >> "$FAKE_INSTALLED"
    done ;;
  "dpkg-query -W") sed 's/$/\tinstalled/' "$FAKE_INSTALLED" ;;
  "apt-get install")
    shift 3
    for p in "$@"; do
      missing "$p" && { echo "E: Unable to locate package $p" >&2; exit 100; }
      echo "$p" >> "$FAKE_INSTALLED"
    done ;;
  "apt-get remove")
    shift 2
    for p in "$@"; do grep -vx "$p" "$FAKE_INSTALLED" > "$FAKE_INSTALLED.t"; mv "$FAKE_INSTALLED.t" "$FAKE_INSTALLED"; done ;;
  "apt-get -v") echo "apt 2.7.0" ;;
  "brew list") cat "$FAKE_INSTALLED" ;;
  "brew install")
    shift
    prev=$1
    if [ "$1" = "--cask" ]; then shift; fi
    missing "$1" && { echo "Error: No formulae or casks found for $1." >&2; exit 1; }
    if [ "$prev" = "--cask" ]; then echo "$1" >> "$FAKE_INSTALLED"; exit 0; fi
    [ "$1" = "mise" ] && cp "$0" "$(dirname "$0")/mise"
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
      install) shift 2; for p in "$@"; do
        missing "$p" && { echo "error: flake 'flake:nixpkgs' does not provide attribute '$p'" >&2; exit 1; }
        echo "$p" >> "$FAKE_LOG.nix"; done ;;
      remove) [ -n "$FAKE_NIX_UNKNOWN" ] && { echo "error: unknown element" >&2; exit 1; } ;;
    esac ;;
  "nix --version") echo "nix (Nix) 2.24.9" ;;
  "mise ls") cat "$FAKE_MISE_JSON" ;;
  "mise --version") echo "2026.1.0 macos-arm64" ;;
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
        "mise",
    ] {
        sb.fake_bin(tool, FAKE);
    }
    sb.set_installed(&[]);
    sb.set_repo_packages(&["ghostty", "ripgrep", "vim"]);
    sb.set_nix_json("{\"elements\":{}}");
    sb.set_mise_json("{}");
    sb.set_missing(&[]);
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
        assert!(
            m.contains("[packages.arch]\nghostty = \"installed\"\nripgrep = \"installed\""),
            "{m}"
        );
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
    fn preinstalled_package_is_adopted_and_kept_on_removal() {
        let sb = sandbox("linux:arch");
        sb.set_installed(&["ghostty", "ripgrep"]);
        let first = up(&sb, &[]);
        assert_eq!(first.status, 0, "{}{}", first.stdout, first.stderr);
        assert!(!first.stdout.contains("adopted"), "{}", first.stdout);
        let m = manifest(&sb);
        assert!(
            m.contains("[packages.arch]\nghostty = \"adopted\"\nripgrep = \"adopted\""),
            "{m}"
        );

        sb.write_repo(
            "modules/tools/module.toml",
            "[packages]\narch = [\"ripgrep\"]\n",
        );
        std::fs::write(sb.log_path(), "").unwrap();
        let run = up(&sb, &[]);
        assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
        assert!(
            run.stdout.contains("orphaned   arch: ghostty"),
            "{}",
            run.stdout
        );
        assert!(!sb.log().join("\n").contains("-Rns"), "{:?}", sb.log());
        assert!(!manifest(&sb).contains("ghostty"), "{}", manifest(&sb));

        let again = up(&sb, &[]);
        assert_eq!(again.stdout, "nothing to do\n");
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
        assert!(manifest(&sb).contains("[packages.ubuntu]\nghostty = \"installed\""));
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

mod mise {
    use super::common::Sandbox;
    use super::{manifest, sandbox, up};

    const MODULE: &str = "[packages]\nmise = [\"gh\", \"npm:@anthropic-ai/claude-code\"]\n";
    const RIG_TOML: &str = "# written by rig, edit module.toml instead\n[tools]\ngh = \"latest\"\n\"npm:@anthropic-ai/claude-code\" = \"latest\"\n";
    const ALL_INSTALLED: &str =
        r#"{"gh":[{"installed":true}],"npm:@anthropic-ai/claude-code":[{"installed":true}]}"#;

    fn rig_toml(sb: &Sandbox) -> Option<String> {
        std::fs::read_to_string(sb.root.path().join("config/mise/conf.d/rig.toml")).ok()
    }

    fn mise_calls(sb: &Sandbox) -> Vec<String> {
        sb.log()
            .into_iter()
            .filter(|l| l.starts_with("mise "))
            .collect()
    }

    #[test]
    fn install_then_idle_then_uninstall() {
        let sb = sandbox("linux:arch");
        sb.write_repo("modules/tools/module.toml", MODULE);
        let run = up(&sb, &[]);
        assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
        assert_eq!(
            mise_calls(&sb),
            vec![
                "mise ls --json --installed",
                "mise install gh npm:@anthropic-ai/claude-code"
            ]
        );
        assert_eq!(rig_toml(&sb).as_deref(), Some(RIG_TOML));
        assert!(run.stdout.contains("mise: gh"), "{}", run.stdout);
        let m = manifest(&sb);
        assert!(
            m.contains("[packages.mise]\ngh = \"installed\"\n\"npm:@anthropic-ai/claude-code\" = \"installed\""),
            "{m}"
        );

        sb.set_mise_json(ALL_INSTALLED);
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        assert_eq!(mise_calls(&sb), vec!["mise ls --json --installed"]);

        sb.write_repo("modules/tools/module.toml", "[packages]\nmise = [\"gh\"]\n");
        std::fs::write(sb.log_path(), "").unwrap();
        up(&sb, &[]);
        assert_eq!(
            mise_calls(&sb),
            vec![
                "mise ls --json --installed",
                "mise uninstall npm:@anthropic-ai/claude-code"
            ]
        );
        assert_eq!(
            rig_toml(&sb).as_deref(),
            Some("# written by rig, edit module.toml instead\n[tools]\ngh = \"latest\"\n")
        );

        sb.write_repo("modules/tools/module.toml", "[packages]\nmise = []\n");
        up(&sb, &[]);
        assert_eq!(rig_toml(&sb), None);
    }

    #[test]
    fn preinstalled_tool_is_adopted_and_never_removed() {
        let sb = sandbox("linux:arch");
        sb.write_repo("modules/tools/module.toml", MODULE);
        sb.set_mise_json(r#"{"npm:@anthropic-ai/claude-code":[{"installed":true}]}"#);
        up(&sb, &[]);
        let m = manifest(&sb);
        assert!(
            m.contains("[packages.mise]\ngh = \"installed\"\n\"npm:@anthropic-ai/claude-code\" = \"adopted\""),
            "{m}"
        );

        sb.write_repo("modules/tools/module.toml", "[packages]\nmise = []\n");
        std::fs::write(sb.log_path(), "").unwrap();
        let run = up(&sb, &[]);
        assert!(
            !sb.log().iter().any(|l| l.starts_with("mise uninstall")),
            "{:?}",
            sb.log()
        );
        assert!(
            run.stdout
                .contains("orphaned   mise: npm:@anthropic-ai/claude-code"),
            "{}",
            run.stdout
        );
        assert!(!manifest(&sb).contains("claude-code"), "{}", manifest(&sb));
    }

    #[test]
    fn dry_run_leaves_config_alone() {
        let sb = sandbox("linux:arch");
        sb.write_repo("modules/tools/module.toml", MODULE);
        up(&sb, &["-n"]);
        assert_eq!(rig_toml(&sb), None);
        assert_eq!(mise_calls(&sb), vec!["mise ls --json --installed"]);
    }

    #[test]
    fn brew_installs_mise_then_mise_installs_tools() {
        let sb = sandbox("macos");
        sb.remove_fake_bin("mise");
        sb.write_repo(
            "modules/tools/module.toml",
            "[packages]\nmacos = [\"mise\"]\nmise = [\"gh\"]\n",
        );
        let run = up(&sb, &[]);
        assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
        let log = sb.log().join("\n");
        assert!(log.contains("brew install mise"), "{log}");
        assert!(log.contains("mise install"), "{log}");
        assert!(run.stdout.contains("mise: gh"), "{}", run.stdout);
    }

    #[test]
    fn unavailable_row_when_mise_missing() {
        let sb = sandbox("linux:arch");
        sb.remove_fake_bin("mise");
        sb.write_repo("modules/tools/module.toml", MODULE);
        let run = up(&sb, &[]);
        assert!(run.stdout.contains("packages: mise"), "{}", run.stdout);
        assert!(run.stdout.contains("not on PATH"), "{}", run.stdout);
    }

    #[test]
    fn doctor_reports_version_and_path() {
        let sb = sandbox("linux:arch");
        let run = sb.rig(&["doctor", sb.repo.to_str().unwrap()]);
        assert!(run.stdout.contains("mise: 2026.1.0"), "{}", run.stdout);
        assert!(
            run.stderr.contains("mise: tools not on PATH"),
            "{}",
            run.stderr
        );

        sb.remove_fake_bin("mise");
        let run = sb.rig(&["doctor", sb.repo.to_str().unwrap()]);
        assert!(run.stdout.contains("mise: not available"), "{}", run.stdout);
        assert!(!run.stderr.contains("mise:"), "{}", run.stderr);
    }
}

mod canonical {
    use super::{sandbox, up};

    const SPELLINGS: &str =
        "[fd]\nubuntu = \"fd-find\"\n\n[gh]\nmise = \"gh\"\n\n[localsend]\nubuntu = false\n";

    fn setup(os: &str, names: &str) -> super::common::Sandbox {
        let sb = sandbox(os);
        sb.write_repo(
            "modules/tools/module.toml",
            &format!("packages = [{names}]\n"),
        );
        sb.write_repo("packages.toml", SPELLINGS);
        sb
    }

    #[test]
    fn spelling_per_os() {
        let sb = setup("linux:ubuntu", "\"fd\"");
        up(&sb, &[]);
        assert!(
            sb.log()
                .join("\n")
                .contains("apt-get install -y --no-install-recommends fd-find"),
            "{:?}",
            sb.log()
        );

        let sb = setup("linux:arch", "\"fd\"");
        sb.set_repo_packages(&["fd"]);
        up(&sb, &[]);
        assert!(
            sb.log()
                .join("\n")
                .contains("pacman -S --needed --noconfirm fd"),
            "{:?}",
            sb.log()
        );
    }

    #[test]
    fn mise_entry_wins_on_every_os() {
        for os in ["linux:arch", "linux:ubuntu", "macos"] {
            let sb = setup(os, "\"gh\"");
            let run = up(&sb, &[]);
            assert_eq!(run.status, 0, "{}{}", run.stdout, run.stderr);
            let log = sb.log().join("\n");
            assert!(log.contains("mise install gh"), "{os}: {log}");
            assert!(
                !log.contains("brew install gh") && !log.contains("noconfirm gh"),
                "{os}: {log}"
            );
        }
    }

    #[test]
    fn absent_is_verbose_only() {
        let sb = setup("linux:ubuntu", "\"localsend\"");
        let run = up(&sb, &[]);
        assert!(!run.stdout.contains("localsend"), "{}", run.stdout);
        assert!(!sb.log().join("\n").contains("localsend"), "{:?}", sb.log());
        let run = up(&sb, &["-v"]);
        assert!(
            run.stdout.contains("package")
                && run.stdout.contains("localsend")
                && run.stdout.contains("(not on ubuntu)"),
            "{}",
            run.stdout
        );
    }

    #[test]
    fn unknown_backend_in_spellings_is_a_load_error() {
        let sb = setup("linux:arch", "\"fd\"");
        sb.write_repo("packages.toml", "[fd]\nfedora = \"fd\"\n");
        let run = up(&sb, &[]);
        assert_eq!(run.status, 2, "{}{}", run.stdout, run.stderr);
        assert!(
            run.stderr.contains("packages.toml") && run.stderr.contains("fedora"),
            "{}",
            run.stderr
        );
    }

    #[test]
    fn not_found_points_at_spellings() {
        for (os, name) in [
            ("linux:ubuntu", "fd"),
            ("linux:arch", "fd"),
            ("macos", "fd"),
        ] {
            let sb = setup(os, &format!("\"{name}\""));
            sb.write_repo("packages.toml", "");
            sb.set_missing(&["fd"]);
            let run = up(&sb, &[]);
            assert_eq!(run.status, 2, "{os}: {}{}", run.stdout, run.stderr);
            let backend = os.rsplit(':').next().unwrap();
            assert!(run.stdout.contains("package fd"), "{os}: {}", run.stdout);
            assert!(
                run.stdout.contains(&format!(
                    "not found on {backend} (add [fd] {backend} = \"...\" to packages.toml)"
                )),
                "{os}: {}",
                run.stdout
            );
            assert!(
                !run.stdout.contains("Unable to locate"),
                "{os}: {}",
                run.stdout
            );
        }
        let sb = setup("linux:arch", "\"fd\"");
        sb.write_repo("packages.toml", "[fd]\nnix = \"nixpkgs#fd\"\n");
        sb.set_missing(&["nixpkgs#fd"]);
        let run = up(&sb, &[]);
        assert!(run.stdout.contains("not found on nix"), "{}", run.stdout);
    }
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
    assert!(!sb.state_dir().join("manifest.toml").exists());

    up(&sb, &[]);
    let before = manifest(&sb);
    sb.write_repo(
        "modules/tools/module.toml",
        "[packages]\narch = [\"vim\"]\n",
    );
    up(&sb, &["-n"]);
    assert_eq!(manifest(&sb), before);
}

#[test]
fn declined_prompt_still_records_adoptions() {
    let sb = sandbox("linux:arch");
    sb.set_installed(&["ghostty"]);
    let repo = sb.repo.to_str().unwrap().to_string();
    let run = sb.rig(&["up", "--host", "box", &repo]);
    assert_eq!(run.status, 1, "{}{}", run.stdout, run.stderr);
    assert!(
        manifest(&sb).contains("ghostty = \"adopted\""),
        "{}",
        manifest(&sb)
    );
}

#[test]
fn reinstalling_an_adopted_package_keeps_it_adopted() {
    let sb = sandbox("linux:arch");
    sb.set_installed(&["ghostty", "ripgrep"]);
    up(&sb, &[]);
    sb.set_installed(&["ripgrep"]);
    up(&sb, &[]);
    assert!(
        manifest(&sb).contains("ghostty = \"adopted\""),
        "{}",
        manifest(&sb)
    );
}

#[test]
fn adopted_blob_survives_the_package_phase() {
    let sb = sandbox("linux:arch");
    sb.set_installed(&["ghostty", "ripgrep"]);
    sb.write_home(".config/tools.conf", "answer = 41\n");
    let run = up(&sb, &["--adopt"]);
    let note = run
        .stdout
        .lines()
        .find(|l| l.contains("~/.config/tools.conf"))
        .unwrap();
    let hash8 = note.rsplit(' ').next().unwrap();
    let blobs: Vec<String> = std::fs::read_dir(sb.state_dir().join("blobs"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    let kept = blobs.iter().find(|b| b.starts_with(hash8)).unwrap();
    assert_eq!(
        std::fs::read_to_string(sb.state_dir().join("blobs").join(kept)).unwrap(),
        "answer = 41\n"
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
