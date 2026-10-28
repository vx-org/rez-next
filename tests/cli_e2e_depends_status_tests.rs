//! CLI end-to-end regressions for `depends` / `status` repository enumeration.
//!
//! These pin the "enumerate the whole repository" contract for the three call
//! sites named in issue #366: `rez depends`, `rez status --families` and bare
//! `rez status`. They all used to enumerate with `find_packages("")`, an exact
//! cache-key lookup: `""` is not a package family, so it returned an empty
//! vector and the commands reported "no packages" with exit 0 even when the
//! repository held packages with known dependents. The fix enumerates families
//! with `list_packages()` and fetches each by name, and a package definition
//! that cannot be loaded is surfaced as an error rather than a fabricated
//! empty success.
//!
//! Every `*_must_*` assertion below is chosen so it FAILS if a call site
//! reverts to the `find_packages("")` idiom (old head) and PASSES on the
//! fixed code (new head). The empty-repository cases guard the legitimate
//! "nothing depends on X" / "no packages" result so a future fix cannot pass
//! by always printing a non-empty answer.

use std::fs;
use std::path::{Path, PathBuf};

// The shared helper module is compiled per test binary; only the helpers this
// file uses are referenced here, so the rest are dead code in this binary.
#[allow(dead_code)]
#[path = "cli_e2e_helpers.rs"]
mod cli_e2e_helpers;

use cli_e2e_helpers::{rez_output, write_package};

macro_rules! require_binary {
    () => {
        if cli_e2e_helpers::skip_if_no_binary() {
            eprintln!("[SKIP] rez-next binary not found. Run `cargo build` first.");
            return;
        }
    };
}

/// Write a package definition that declares runtime requirements.
fn write_with_requires(repo: &Path, name: &str, version: &str, requires: &[&str]) {
    let listed = requires
        .iter()
        .map(|r| format!("    \"{r}\","))
        .collect::<Vec<_>>()
        .join("\n");
    write_package(repo, name, version, &format!("requires = [\n{listed}\n]\n"));
}

/// A repository with a known dependent (two `maya` versions both require
/// `python`) plus a `standalone` package that depends on nothing.
fn repo_with_known_dependent(base: &Path) -> PathBuf {
    let repo = base.join("packages");
    write_package(&repo, "python", "3.11.0", "");
    write_with_requires(&repo, "maya", "2024.1", &["python-3+"]);
    write_with_requires(&repo, "maya", "2023.1", &["python-3"]);
    write_package(&repo, "standalone", "1.0.0", "");
    repo
}

/// A repository whose `maya` definition is intentionally malformed, so any
/// attempt to read it must fail closed rather than report an empty package.
fn repo_with_malformed_definition(base: &Path) -> PathBuf {
    let repo = base.join("packages");
    write_package(&repo, "python", "3.11.0", "");
    let dir = repo.join("maya").join("2024.1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("package.py"),
        "name = 'maya'\nversion = '2024.1'\nthis is not valid python !!!\n",
    )
    .unwrap();
    repo
}

// ── depends ───────────────────────────────────────────────────────────────────

/// A package with a known dependent must be reported by `rez depends`.
/// Under the old idiom this printed "No packages depend on 'python'" and
/// exited 0 — a wrong answer that looked clean in CI.
#[test]
fn depends_must_report_a_known_dependent() {
    require_binary!();
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_known_dependent(tmp.path());

    let (stdout, stderr, code) =
        rez_output(&["depends", "python", "--paths", repo.to_str().unwrap()]);

    assert_eq!(
        code,
        Some(0),
        "depends must succeed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("maya"),
        "maya depends on python and must be listed: {stdout}"
    );
    assert!(
        !stdout.contains("standalone"),
        "a package without the dependency must not be listed: {stdout}"
    );
    assert!(
        !stdout.contains("No packages depend on"),
        "must not fabricate an empty success: {stdout}"
    );
}

/// An empty repository is a genuine "nothing depends on X" result, not an
/// error. This guards against a "fix" that always prints a non-empty answer.
#[test]
fn depends_empty_repository_must_report_no_dependents() {
    require_binary!();
    let tmp = tempfile::tempdir().unwrap();

    let (stdout, stderr, code) =
        rez_output(&["depends", "python", "--paths", tmp.path().to_str().unwrap()]);

    assert_eq!(
        code,
        Some(0),
        "an empty repository is not an error: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("No packages depend on 'python'"),
        "empty repository should report no dependents: {stdout}"
    );
}

/// A definition that fails to load must surface as an error, never a
/// fabricated "no dependents" success. The enumeration must fail closed.
#[test]
fn depends_must_surface_a_malformed_definition_as_error() {
    require_binary!();
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_malformed_definition(tmp.path());

    let (stdout, stderr, code) =
        rez_output(&["depends", "python", "--paths", repo.to_str().unwrap()]);
    let combined = format!("{stdout}{stderr}");

    assert_ne!(
        code,
        Some(0),
        "a broken definition must not exit 0: {combined}"
    );
    assert!(
        !combined.contains("No packages depend on"),
        "a load error must not be hidden behind an empty success: {combined}"
    );
}

// ── status --families ─────────────────────────────────────────────────────────

/// `rez status --families` must list every family in the repository. The old
/// idiom printed "No packages found" with exit 0.
#[test]
fn status_families_must_list_every_family() {
    require_binary!();
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_known_dependent(tmp.path());

    let (stdout, stderr, code) =
        rez_output(&["status", "--families", "--paths", repo.to_str().unwrap()]);

    assert_eq!(
        code,
        Some(0),
        "status --families must succeed: stdout={stdout} stderr={stderr}"
    );
    for family in ["maya", "python", "standalone"] {
        assert!(
            stdout.contains(family),
            "family '{family}' must be listed: {stdout}"
        );
    }
    assert!(
        !stdout.contains("No packages found"),
        "must not fabricate an empty repository: {stdout}"
    );
}

/// An empty repository is a legitimate "No packages found" result for
/// `status --families`, not an error.
#[test]
fn status_families_empty_repository_must_report_no_packages() {
    require_binary!();
    let tmp = tempfile::tempdir().unwrap();

    let (stdout, stderr, code) = rez_output(&[
        "status",
        "--families",
        "--paths",
        tmp.path().to_str().unwrap(),
    ]);

    assert_eq!(
        code,
        Some(0),
        "an empty repository is not an error: stdout={stdout} stderr={stderr}"
    );
    assert!(
        stdout.contains("No packages found"),
        "empty repository should report no packages: {stdout}"
    );
}

/// `status --families` must fail closed on a broken definition rather than
/// silently reporting zero families.
#[test]
fn status_families_must_surface_a_malformed_definition_as_error() {
    require_binary!();
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_malformed_definition(tmp.path());

    let (stdout, stderr, code) =
        rez_output(&["status", "--families", "--paths", repo.to_str().unwrap()]);
    let combined = format!("{stdout}{stderr}");

    assert_ne!(
        code,
        Some(0),
        "a broken definition must not exit 0: {combined}"
    );
    assert!(
        !combined.contains("No packages found"),
        "a load error must not be hidden behind an empty result: {combined}"
    );
}

// ── bare status ───────────────────────────────────────────────────────────────

/// Bare `rez status` must report non-zero package and family counts. The old
/// idiom reported zeros for a fully populated repository.
#[test]
fn status_overview_must_count_packages_and_families() {
    require_binary!();
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_known_dependent(tmp.path());

    let (stdout, stderr, code) = rez_output(&["status", "--paths", repo.to_str().unwrap()]);

    assert_eq!(
        code,
        Some(0),
        "bare status must succeed: stdout={stdout} stderr={stderr}"
    );
    assert!(
        !stdout.contains("Total packages: 0"),
        "a populated repository must not report zero packages: {stdout}"
    );
    assert!(
        !stdout.contains("Package families: 0"),
        "a populated repository must not report zero families: {stdout}"
    );
    // The controlled repository holds exactly three families (python, maya,
    // standalone) across four versions. An exact count also fails if the
    // command enumerates only a single (empty-string) "family".
    assert!(
        stdout.contains("Package families: 3"),
        "all three families must be counted: {stdout}"
    );
    assert!(
        stdout.contains("Total packages: 4"),
        "all four versions must be counted: {stdout}"
    );
}
