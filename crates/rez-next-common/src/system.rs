//! Cross-platform host identifiers using Rez-compatible spellings.

use serde::{Deserialize, Serialize};

/// Stable system identifiers used by implicit package requests and embedders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemInfo {
    pub platform: String,
    pub arch: String,
    pub os: String,
}

impl SystemInfo {
    /// Detect the current host using Rez's platform and architecture spelling.
    #[must_use]
    pub fn detect() -> Self {
        Self {
            platform: detect_platform(),
            arch: detect_arch(),
            os: detect_os(),
        }
    }

    /// Requirements suitable for a system-dependent package variant.
    #[must_use]
    pub fn variant(&self) -> [String; 3] {
        [
            format!("platform-{}", self.platform),
            format!("arch-{}", self.arch),
            format!("os-{}", self.os),
        ]
    }

    /// Default weak requests appended to Rez resolves.
    #[must_use]
    pub fn implicit_packages(&self) -> [String; 3] {
        [
            format!("~platform=={}", self.platform),
            format!("~arch=={}", self.arch),
            format!("~os=={}", self.os),
        ]
    }
}

/// Return host variables required to start native Windows child processes.
///
/// These variables are deliberately supplied only when a caller builds an
/// explicit child environment. They are not part of a resolved Rez
/// environment and must not weaken environment isolation.
#[must_use]
#[cfg(target_os = "windows")]
pub fn windows_process_environment() -> Vec<(String, String)> {
    let host_environment: Vec<_> = std::env::vars().collect();
    ["SystemRoot", "WINDIR"]
        .into_iter()
        .filter_map(|required_name| {
            host_environment
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(required_name))
                .cloned()
        })
        .collect()
}

/// Return no host bootstrap variables on platforms without Windows process
/// startup requirements.
#[must_use]
#[cfg(not(target_os = "windows"))]
pub fn windows_process_environment() -> Vec<(String, String)> {
    Vec::new()
}

fn detect_platform() -> String {
    match std::env::consts::OS {
        "macos" => "osx".to_string(),
        other => other.to_string(),
    }
}

fn detect_arch() -> String {
    match std::env::consts::ARCH {
        "aarch64" => "arm_64".to_string(),
        other => other.to_string(),
    }
}

#[cfg(target_os = "linux")]
fn detect_os() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|content| {
            content.lines().find_map(|line| {
                line.strip_prefix("PRETTY_NAME=")
                    .map(|value| value.trim_matches('"'))
                    .filter(|value| !value.is_empty())
                    .map(make_safe_version_string)
            })
        })
        .unwrap_or_else(|| "linux".to_string())
}

#[cfg(target_os = "windows")]
fn detect_os() -> String {
    "windows".to_string()
}

#[cfg(target_os = "macos")]
fn detect_os() -> String {
    "osx".to_string()
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn detect_os() -> String {
    make_safe_version_string(std::env::consts::OS)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn make_safe_version_string(value: &str) -> String {
    value
        .trim_matches(['.', '-'])
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detected_system_produces_three_rez_implicit_requests() {
        let system = SystemInfo::detect();
        let implicits = system.implicit_packages();

        assert_eq!(implicits[0], format!("~platform=={}", system.platform));
        assert_eq!(implicits[1], format!("~arch=={}", system.arch));
        assert_eq!(implicits[2], format!("~os=={}", system.os));
        assert!(implicits.iter().all(|request| !request.contains(' ')));
    }
}
