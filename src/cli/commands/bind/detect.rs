//! System tool detection for the bind command.
//!
//! Executable-backed binders are detected by `rez-next-bind`, which owns the
//! candidate list and version contract. The CLI keeps detection for Rez's
//! logical platform packages here because those packages have no executable.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use super::utils::{extract_version_from_output, parse_version_from_string, which_executable};
use rez_next_bind::detect_builtin_tool;

/// Detected system tool information.
#[derive(Debug, Clone)]
pub struct DetectedTool {
    /// Detected version string.
    pub version: String,
    /// Executable path.
    pub executable_path: Option<PathBuf>,
    /// Additional metadata.
    pub metadata: HashMap<String, String>,
}

/// Dispatch to the appropriate detector based on `name`.
pub fn detect_system_tool(name: &str) -> Option<DetectedTool> {
    match name {
        "setuptools" => detect_setuptools(),
        "platform" | "arch" | "os" | "rez" => detect_platform_package(name),
        _ => detect_builtin_tool(name).ok().map(|detected| DetectedTool {
            version: detected.version,
            executable_path: Some(detected.executable_path),
            metadata: HashMap::new(),
        }),
    }
}

fn detect_setuptools() -> Option<DetectedTool> {
    let script = "import setuptools; print(setuptools.__version__)";
    let cmd = if cfg!(windows) { "python" } else { "python3" };
    if let Ok(output) = Command::new(cmd).args(["-c", script]).output()
        && output.status.success()
    {
        let version_output = String::from_utf8_lossy(&output.stdout).to_string()
            + &String::from_utf8_lossy(&output.stderr);
        let decorated_output = format!("setuptools {version_output}");
        if let Some(ver) = extract_version_from_output(&decorated_output, "setuptools") {
            return Some(DetectedTool {
                version: ver,
                executable_path: which_executable(cmd),
                metadata: HashMap::new(),
            });
        }
    }
    None
}

fn detect_platform_package(name: &str) -> Option<DetectedTool> {
    match name {
        "platform" => {
            let platform = if cfg!(windows) {
                "windows"
            } else if cfg!(target_os = "macos") {
                "osx"
            } else {
                "linux"
            };
            let mut metadata = HashMap::new();
            metadata.insert("platform".to_string(), platform.to_string());
            Some(DetectedTool {
                version: "1.0.0".to_string(),
                executable_path: None,
                metadata,
            })
        }
        "arch" => {
            let mut metadata = HashMap::new();
            metadata.insert("arch".to_string(), std::env::consts::ARCH.to_string());
            Some(DetectedTool {
                version: "1.0.0".to_string(),
                executable_path: None,
                metadata,
            })
        }
        "os" => Some(DetectedTool {
            version: get_os_version(),
            executable_path: None,
            metadata: HashMap::new(),
        }),
        "rez" => Some(DetectedTool {
            version: env!("CARGO_PKG_VERSION").to_string(),
            executable_path: std::env::current_exe().ok(),
            metadata: HashMap::new(),
        }),
        _ => None,
    }
}

/// Return a best-effort OS version string.
pub(crate) fn get_os_version() -> String {
    if cfg!(windows) {
        if let Ok(output) = Command::new("cmd").args(["/c", "ver"]).output() {
            let s = String::from_utf8_lossy(&output.stdout).to_string();
            if let Some(ver) = parse_version_from_string(&s) {
                return ver;
            }
        }
        "10.0".to_string()
    } else if cfg!(target_os = "macos") {
        if let Ok(output) = Command::new("sw_vers").args(["-productVersion"]).output() {
            return String::from_utf8_lossy(&output.stdout).trim().to_string();
        }
        "unknown".to_string()
    } else if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
        for line in content.lines() {
            if line.starts_with("VERSION_ID=") {
                return line
                    .trim_start_matches("VERSION_ID=")
                    .trim_matches('"')
                    .to_string();
            }
        }
        "unknown".to_string()
    } else {
        "unknown".to_string()
    }
}
