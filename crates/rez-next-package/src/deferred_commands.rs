//! Internal encoding for package-command conditions that depend on a resolved context.

use rez_next_common::RezCoreError;
use std::collections::HashSet;

const DEFERRED_PREFIX: &str = "__rez_next_deferred__\t";

pub fn defer_resolve_membership(
    commands: &str,
    package_name: &str,
    expected_present: bool,
) -> String {
    defer(commands, "resolve", expected_present, package_name)
}

pub fn defer_build_mode(commands: &str, expected: bool) -> String {
    defer(commands, "building", expected, "")
}

pub fn defer_unsupported_build_mode() -> String {
    defer(
        "dynamic package command block",
        "unsupported-building",
        true,
        "",
    )
}

pub fn defer_unsupported_build_or_ephemeral(ephemeral_name: &str) -> String {
    defer(
        "dynamic package command block",
        "unsupported-build-or-ephemeral",
        true,
        ephemeral_name,
    )
}

pub fn materialize_deferred_commands(
    commands: &str,
    resolved_package_names: &HashSet<String>,
    resolved_ephemeral_names: &HashSet<String>,
    building: bool,
) -> Result<String, RezCoreError> {
    commands
        .lines()
        .filter_map(|line| {
            materialize_line(
                line,
                resolved_package_names,
                resolved_ephemeral_names,
                building,
            )
            .transpose()
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|lines| lines.join("\n"))
}

fn defer(commands: &str, kind: &str, expected: bool, argument: &str) -> String {
    commands
        .lines()
        .map(|command| {
            format!(
                "{DEFERRED_PREFIX}{kind}\t{}\t{argument}\t{command}",
                u8::from(expected)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn materialize_line(
    line: &str,
    resolved_package_names: &HashSet<String>,
    resolved_ephemeral_names: &HashSet<String>,
    building: bool,
) -> Result<Option<String>, RezCoreError> {
    let Some(encoded) = line.strip_prefix(DEFERRED_PREFIX) else {
        return Ok(Some(line.to_string()));
    };
    let mut fields = encoded.splitn(4, '\t');
    let kind = fields.next();
    let expected = fields.next();
    let argument = fields.next();
    let command = fields.next();
    let (Some(kind), Some(expected), Some(argument), Some(command)) =
        (kind, expected, argument, command)
    else {
        return Err(RezCoreError::PackageParse(
            "Malformed deferred package command".to_string(),
        ));
    };
    let expected = match expected {
        "1" => true,
        "0" => false,
        _ => {
            return Err(RezCoreError::PackageParse(
                "Malformed deferred package command condition".to_string(),
            ));
        }
    };
    if kind == "unsupported-build-or-ephemeral" {
        if building || resolved_ephemeral_names.contains(argument) {
            return Err(RezCoreError::ExecutionError(format!(
                "Dynamic package command block is not supported for build or '.{argument}' contexts"
            )));
        }
        return Ok(None);
    }
    if kind == "unsupported-building" {
        if building {
            return Err(RezCoreError::ExecutionError(
                "Dynamic package command block is not supported in build contexts".to_string(),
            ));
        }
        return Ok(None);
    }
    let actual = match kind {
        "resolve" => resolved_package_names.contains(argument),
        "building" => building,
        _ => {
            return Err(RezCoreError::PackageParse(format!(
                "Unknown deferred package command condition: {kind}"
            )));
        }
    };
    if actual == expected {
        materialize_line(
            command,
            resolved_package_names,
            resolved_ephemeral_names,
            building,
        )
    } else {
        Ok(None)
    }
}
