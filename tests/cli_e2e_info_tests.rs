//! End-to-end contract tests for machine-readable installation discovery.

#[path = "cli_e2e_helpers.rs"]
#[allow(dead_code)]
mod cli_e2e_helpers;

use std::env;
use std::path::Path;
use std::process::Command;

#[test]
fn info_json_reports_configured_paths_without_requiring_legacy_rez() {
    skip_no_bin!();

    let temp = tempfile::tempdir().unwrap();
    let empty_path = temp.path().join("empty-path");
    let packages_a = temp.path().join("packages-a");
    let packages_b = temp.path().join("packages-b");
    let local_packages = temp.path().join("local");
    let release_packages = temp.path().join("release");
    let config_file = temp.path().join("rezconfig.yaml");

    std::fs::create_dir_all(&empty_path).unwrap();
    std::fs::write(&config_file, "default_shell: test-shell\n").unwrap();

    let packages_path = env::join_paths([&packages_a, &packages_b]).unwrap();
    let output = Command::new(cli_e2e_helpers::rez_next_bin())
        .args(["info", "--json"])
        .env("PATH", &empty_path)
        .env("REZ_CONFIG_FILE", &config_file)
        .env("REZ_PACKAGES_PATH", &packages_path)
        .env("REZ_LOCAL_PACKAGES_PATH", &local_packages)
        .env("REZ_RELEASE_PACKAGES_PATH", &release_packages)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "info --json should not require legacy Rez\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["rez_next_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(info["rez_legacy_available"], false);
    assert!(info["rez_legacy_path"].is_null());
    assert_eq!(
        info["packages_path"],
        serde_json::json!([packages_a.to_string_lossy(), packages_b.to_string_lossy()])
    );
    assert_eq!(
        info["local_packages_path"],
        local_packages.to_string_lossy().as_ref()
    );
    assert_eq!(
        info["release_packages_path"],
        release_packages.to_string_lossy().as_ref()
    );
    assert_eq!(info["config_file"], config_file.to_string_lossy().as_ref());
    assert!(info.get("python_version").is_some());
}

#[test]
fn info_json_does_not_report_a_rez_next_alias_as_legacy_rez() {
    skip_no_bin!();

    let temp = tempfile::tempdir().unwrap();
    let alias = temp
        .path()
        .join(if cfg!(windows) { "rez.exe" } else { "rez" });
    std::fs::copy(cli_e2e_helpers::rez_next_bin(), &alias).unwrap();
    ensure_executable(&alias);

    let output = Command::new(cli_e2e_helpers::rez_next_bin())
        .args(["info", "--json"])
        .env("PATH", temp.path())
        .env("PATHEXT", ".COM;.EXE;.BAT;.CMD")
        .output()
        .unwrap();

    assert!(output.status.success());
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["rez_legacy_available"], false);
    assert!(info["rez_legacy_path"].is_null());
}

#[test]
fn info_json_reports_a_legacy_rez_found_on_path() {
    skip_no_bin!();

    let temp = tempfile::tempdir().unwrap();
    let legacy_rez = write_legacy_rez(temp.path());
    let output = Command::new(cli_e2e_helpers::rez_next_bin())
        .args(["info", "--json"])
        .env("PATH", temp.path())
        .env("PATHEXT", ".COM;.EXE;.BAT;.CMD")
        .output()
        .unwrap();

    assert!(output.status.success());
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info["rez_legacy_available"], true);
    let detected = info["rez_legacy_path"].as_str().unwrap();
    assert_same_path(detected, &legacy_rez);
}

#[test]
fn which_reports_the_running_rez_next_executable() {
    skip_no_bin!();

    let output = Command::new(cli_e2e_helpers::rez_next_bin())
        .arg("which")
        .output()
        .unwrap();

    assert!(output.status.success());
    let detected = String::from_utf8_lossy(&output.stdout);
    assert_same_path(detected.trim(), &cli_e2e_helpers::rez_next_bin());
}

#[test]
fn which_rez_reports_legacy_rez_and_fails_when_it_is_absent() {
    skip_no_bin!();

    let temp = tempfile::tempdir().unwrap();
    let legacy_rez = write_legacy_rez(temp.path());
    let found = Command::new(cli_e2e_helpers::rez_next_bin())
        .args(["which", "--rez"])
        .env("PATH", temp.path())
        .env("PATHEXT", ".COM;.EXE;.BAT;.CMD")
        .output()
        .unwrap();
    assert!(found.status.success());
    assert_same_path(String::from_utf8_lossy(&found.stdout).trim(), &legacy_rez);

    let empty_path = temp.path().join("empty");
    std::fs::create_dir(&empty_path).unwrap();
    let missing = Command::new(cli_e2e_helpers::rez_next_bin())
        .args(["which", "--rez"])
        .env("PATH", empty_path)
        .env("PATHEXT", ".COM;.EXE;.BAT;.CMD")
        .output()
        .unwrap();
    assert!(!missing.status.success());
}

#[cfg(unix)]
fn write_legacy_rez(directory: &Path) -> std::path::PathBuf {
    let path = directory.join("rez");
    std::fs::write(&path, "#!/bin/sh\necho 'Rez 2.114.0'\n").unwrap();
    ensure_executable(&path);
    path
}

#[cfg(windows)]
fn write_legacy_rez(directory: &Path) -> std::path::PathBuf {
    let path = directory.join("rez.cmd");
    std::fs::write(&path, "@echo off\r\necho Rez 2.114.0\r\n").unwrap();
    path
}

#[cfg(unix)]
fn ensure_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = path.metadata().unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[cfg(windows)]
fn ensure_executable(_path: &Path) {}

#[cfg(unix)]
fn assert_same_path(actual: &str, expected: &Path) {
    assert_eq!(actual, expected.to_string_lossy());
}

#[cfg(windows)]
fn assert_same_path(actual: &str, expected: &Path) {
    assert!(
        actual.eq_ignore_ascii_case(&expected.to_string_lossy()),
        "paths differ: actual={actual}, expected={}",
        expected.display()
    );
}
