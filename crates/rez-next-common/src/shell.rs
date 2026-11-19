//! Shared shell names and platform-neutral shell metadata.

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

/// Shells provided by Rez's built-in shell plugins.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum ShellType {
    /// POSIX `sh`.
    Sh,
    /// GNU Bash.
    Bash,
    /// Z shell.
    Zsh,
    /// Fish shell.
    Fish,
    /// C shell.
    Csh,
    /// TENEX C shell.
    Tcsh,
    /// Git Bash on Windows.
    GitBash,
    /// Windows Command Prompt.
    Cmd,
    /// Windows PowerShell 5.
    PowerShell,
    /// PowerShell Core.
    Pwsh,
}

impl ShellType {
    /// Return the canonical plugin name.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Sh => "sh",
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Csh => "csh",
            Self::Tcsh => "tcsh",
            Self::GitBash => "gitbash",
            Self::Cmd => "cmd",
            Self::PowerShell => "powershell",
            Self::Pwsh => "pwsh",
        }
    }

    /// Return the executable used to launch the shell.
    pub const fn executable(&self) -> &'static str {
        match self {
            Self::Sh => "sh",
            Self::Bash | Self::GitBash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Csh => "csh",
            Self::Tcsh => "tcsh",
            Self::Cmd => "cmd",
            Self::PowerShell => "powershell",
            Self::Pwsh => "pwsh",
        }
    }

    /// Return the activation script extension.
    pub const fn script_extension(&self) -> &'static str {
        match self {
            Self::Sh | Self::Bash | Self::Zsh | Self::GitBash => "sh",
            Self::Fish => "fish",
            Self::Csh | Self::Tcsh => "csh",
            Self::Cmd => "bat",
            Self::PowerShell | Self::Pwsh => "ps1",
        }
    }

    /// Return the command-line flag that evaluates a command string.
    pub const fn command_flag(&self) -> &'static str {
        match self {
            Self::Sh
            | Self::Bash
            | Self::Zsh
            | Self::Fish
            | Self::Csh
            | Self::Tcsh
            | Self::GitBash => "-c",
            Self::Cmd => "/c",
            Self::PowerShell | Self::Pwsh => "-Command",
        }
    }

    /// Whether this shell accepts POSIX-style command and script handling.
    pub const fn is_posix_like(&self) -> bool {
        matches!(
            self,
            Self::Sh | Self::Bash | Self::Zsh | Self::Fish | Self::GitBash
        )
    }

    /// Whether this shell uses C-shell syntax.
    pub const fn is_c_shell(&self) -> bool {
        matches!(self, Self::Csh | Self::Tcsh)
    }

    /// Whether this shell is a PowerShell implementation.
    pub const fn is_power_shell(&self) -> bool {
        matches!(self, Self::PowerShell | Self::Pwsh)
    }

    /// Whether this shell is Windows Command Prompt.
    pub const fn is_cmd(&self) -> bool {
        matches!(self, Self::Cmd)
    }

    /// Return every built-in shell in a stable order.
    pub const fn builtins() -> [Self; 10] {
        [
            Self::Sh,
            Self::Bash,
            Self::Zsh,
            Self::Fish,
            Self::Csh,
            Self::Tcsh,
            Self::GitBash,
            Self::Cmd,
            Self::PowerShell,
            Self::Pwsh,
        ]
    }

    /// Detect the current shell from the parent process environment.
    pub fn detect() -> Self {
        if let Some(shell) = std::env::var_os("SHELL")
            && let Some(shell_type) = Self::from_executable_name(Path::new(&shell))
        {
            return shell_type;
        }

        if cfg!(windows) {
            if std::env::var_os("MSYSTEM").is_some() {
                return Self::GitBash;
            }
            if std::env::var_os("PSModulePath").is_some() {
                return Self::Pwsh;
            }
            Self::Cmd
        } else {
            Self::Bash
        }
    }

    fn from_executable_name(path: &Path) -> Option<Self> {
        let name = path.file_stem()?.to_str()?.to_ascii_lowercase();
        Self::parse(&name)
    }

    /// Parse a shell name or common executable alias.
    pub fn parse(value: &str) -> Option<Self> {
        value.parse().ok()
    }
}

impl FromStr for ShellType {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "sh" => Ok(Self::Sh),
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            "fish" => Ok(Self::Fish),
            "csh" => Ok(Self::Csh),
            "tcsh" => Ok(Self::Tcsh),
            "gitbash" | "git-bash" => Ok(Self::GitBash),
            "cmd" => Ok(Self::Cmd),
            "powershell" | "ps1" => Ok(Self::PowerShell),
            "pwsh" => Ok(Self::Pwsh),
            other => Err(format!("unknown shell type: {other}")),
        }
    }
}
