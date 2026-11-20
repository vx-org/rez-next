//! End-to-end contracts for Rez implicit request controls.

#[path = "cli_e2e_helpers.rs"]
#[allow(dead_code)]
mod cli_e2e_helpers;

#[test]
fn env_help_exposes_the_official_no_implicit_switch() {
    skip_no_bin!();

    let output = cli_e2e_helpers::rez_ok(&["env", "--help"]);

    assert!(output.contains("--no-implicit"));
}
