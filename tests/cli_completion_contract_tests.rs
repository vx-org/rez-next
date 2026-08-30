//! Stable IDE and shell completion contract tests.

use std::fs;
use std::path::Path;
use std::process::Command;

fn write_package(repo: &Path, name: &str, version: &str, extra: &str) {
    let package_dir = repo.join(name).join(version);
    fs::create_dir_all(&package_dir).expect("package directory");
    fs::write(
        package_dir.join("package.py"),
        format!("name = \"{name}\"\nversion = \"{version}\"\n{extra}\n"),
    )
    .expect("package.py");
}

fn completion_repo() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("temporary repository");
    let repo = temp.path().join("packages");
    write_package(
        &repo,
        "zz_completion_contract_python",
        "3.9.0",
        r#"description = "Completion contract interpreter""#,
    );
    write_package(
        &repo,
        "zz_completion_contract_python",
        "3.11.0",
        r#"description = "Completion contract interpreter""#,
    );
    write_package(
        &repo,
        "zz_completion_contract_tools",
        "1.0.0",
        r#"description = "Completion contract tools""#,
    );
    temp
}

fn run_completion(repo: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_rez-next"))
        .env("REZ_PACKAGES_PATH", repo)
        .args(args)
        .output()
        .expect("rez-next completion command should run")
}

#[test]
fn package_completion_json_is_versioned_grouped_and_sorted() {
    let temp = completion_repo();
    let repo = temp.path().join("packages");
    let output = run_completion(
        &repo,
        &[
            "completions",
            "packages",
            "--prefix",
            "zz_completion_contract_p",
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stable completion JSON");
    assert_eq!(response["schema_version"], 1);
    assert_eq!(response["kind"], "packages");
    assert_eq!(response["packages"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        response["packages"][0]["name"],
        "zz_completion_contract_python"
    );
    assert_eq!(
        response["packages"][0]["versions"],
        serde_json::json!(["3.11.0", "3.9.0"])
    );
    assert_eq!(
        response["packages"][0]["description"],
        "Completion contract interpreter"
    );
}

#[test]
fn version_completion_json_uses_rez_packages_path() {
    let temp = completion_repo();
    let repo = temp.path().join("packages");
    let output = run_completion(
        &repo,
        &[
            "completions",
            "versions",
            "--package",
            "zz_completion_contract_python",
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let response: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stable completion JSON");
    assert_eq!(response["schema_version"], 1);
    assert_eq!(response["kind"], "versions");
    assert_eq!(response["package"], "zz_completion_contract_python");
    assert_eq!(response["versions"], serde_json::json!(["3.11.0", "3.9.0"]));
}

#[test]
fn shell_completion_registration_is_generated_for_all_supported_shells() {
    for (shell, marker) in [
        ("bash", "REZ_NEXT_COMPLETE"),
        ("zsh", "REZ_NEXT_COMPLETE"),
        ("fish", "REZ_NEXT_COMPLETE"),
        ("powershell", "REZ_NEXT_COMPLETE"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_rez-next"))
            .args(["completions", "shell", "--shell", shell])
            .output()
            .expect("shell completion generation should run");
        assert!(
            output.status.success(),
            "{shell} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let script = String::from_utf8_lossy(&output.stdout);
        assert!(script.contains(marker), "{shell} script: {script}");
        assert!(script.contains("rez-next"), "{shell} script: {script}");
    }
}

#[test]
fn dynamic_shell_engine_queries_package_and_version_candidates() {
    let temp = completion_repo();
    let repo = temp.path().join("packages");

    for (prefix, expected) in [
        ("zz_completion_contract_p", "zz_completion_contract_python"),
        (
            "zz_completion_contract_python-3.",
            "zz_completion_contract_python-3.11.0",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_rez-next"))
            .env("REZ_PACKAGES_PATH", &repo)
            .env("REZ_NEXT_COMPLETE", "powershell")
            .args(["--", "rez-next", "env", prefix])
            .output()
            .expect("dynamic package completion should run");
        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let candidates = String::from_utf8_lossy(&output.stdout);
        assert!(
            candidates.lines().any(|candidate| candidate == expected),
            "expected {expected:?} in {candidates:?}"
        );
    }
}

#[test]
fn dynamic_shell_engine_lists_the_complete_command_surface() {
    let output = Command::new(env!("CARGO_BIN_EXE_rez-next"))
        .env("REZ_NEXT_COMPLETE", "powershell")
        .args(["--", "rez-next", ""])
        .output()
        .expect("dynamic command completion should run");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let candidates = String::from_utf8_lossy(&output.stdout);
    let command_names = candidates
        .lines()
        .filter_map(|line| line.split_once('\t').map(|(name, _)| name))
        .collect::<Vec<_>>();

    for command in [
        "config",
        "info",
        "context",
        "view",
        "which",
        "env",
        "release",
        "test",
        "build",
        "search",
        "bind",
        "depends",
        "solve",
        "cp",
        "mv",
        "rm",
        "status",
        "diff",
        "pkg-help",
        "plugins",
        "pkg-cache",
        "suites",
        "bundle",
        "pip",
        "complete",
        "completions",
        "forward",
        "gui",
        "parse-version",
        "self-test",
        "self-update",
    ] {
        assert!(
            command_names.contains(&command),
            "missing command {command:?} in {candidates:?}"
        );
    }
}

#[test]
fn installers_enable_dynamic_completions_with_an_opt_out() {
    let unix_installer = include_str!("../install.sh");
    for shell in ["bash", "zsh", "fish"] {
        assert!(
            unix_installer.contains(&format!("completions shell --shell {shell}")),
            "install.sh should generate {shell} completions"
        );
    }
    assert!(unix_installer.contains("REZ_NEXT_NO_COMPLETIONS"));

    let powershell_installer = include_str!("../install.ps1");
    assert!(powershell_installer.contains("completions shell --shell powershell"));
    assert!(powershell_installer.contains("$PROFILE.CurrentUserAllHosts"));
    assert!(powershell_installer.contains("REZ_NEXT_NO_COMPLETIONS"));
}
