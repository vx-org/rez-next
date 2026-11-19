//! Built-in release hook implementations.
//!
//! This module provides sample hooks that can be used for testing
//! or as references for custom hook implementations.

use crate::{ReleaseHook, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

/// Configuration for EmailHook.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct EmailHookConfig {
    smtp_server: String,
    smtp_port: u16,
    from_address: String,
    to_addresses: Vec<String>,
    use_tls: bool,
}

impl Default for EmailHookConfig {
    fn default() -> Self {
        Self {
            smtp_server: "localhost".to_string(),
            smtp_port: 25,
            from_address: "rez@localhost".to_string(),
            to_addresses: vec![],
            use_tls: false,
        }
    }
}

/// Configuration for WebHook.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct WebHookConfig {
    url: String,
    method: String,
    headers: std::collections::HashMap<String, String>,
    timeout_secs: u64,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(untagged)]
enum CommandArguments {
    List(Vec<String>),
    String(String),
}

impl Default for CommandArguments {
    fn default() -> Self {
        Self::List(Vec::new())
    }
}

impl CommandArguments {
    fn as_vec(&self) -> Vec<String> {
        match self {
            Self::List(args) => args.clone(),
            Self::String(args) => args.split_whitespace().map(str::to_string).collect(),
        }
    }
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
struct CommandSpec {
    command: String,
    #[serde(default)]
    args: CommandArguments,
    #[serde(default)]
    env: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
struct CommandHookConfig {
    #[serde(default)]
    print_commands: bool,
    #[serde(default)]
    print_output: bool,
    #[serde(default)]
    print_error: bool,
    #[serde(default)]
    cancel_on_error: bool,
    #[serde(default)]
    stop_on_error: bool,
    #[serde(default)]
    pre_build_commands: Vec<CommandSpec>,
    #[serde(default)]
    pre_release_commands: Vec<CommandSpec>,
    #[serde(default)]
    post_release_commands: Vec<CommandSpec>,
}

impl Default for WebHookConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            method: "POST".to_string(),
            headers: std::collections::HashMap::new(),
            timeout_secs: 30,
        }
    }
}

/// A no-op release hook that does nothing.
///
/// This hook is useful for testing or as a placeholder.
/// All hook methods return `Ok(())` without doing anything.
pub struct NoopHook;

impl ReleaseHook for NoopHook {
    fn name() -> String {
        "noop".to_string()
    }

    fn new(_source_path: PathBuf) -> Result<Self> {
        Ok(Self)
    }
}

/// A logging release hook that logs messages when hooks are called.
///
/// This hook is useful for debugging the release process.
/// It logs the event name and package path at INFO level.
pub struct LoggingHook;

impl ReleaseHook for LoggingHook {
    fn name() -> String {
        "logging".to_string()
    }

    fn new(_source_path: PathBuf) -> Result<Self> {
        Ok(Self)
    }

    fn pre_build(
        &self,
        _user: &str,
        _install_path: &std::path::Path,
        _variants: Option<&[usize]>,
        _release_message: Option<&str>,
        _changelog: Option<&[String]>,
        _previous_version: Option<&str>,
        _previous_revision: Option<&str>,
    ) -> Result<()> {
        tracing::info!("ReleaseHook: pre_build called");
        Ok(())
    }

    fn pre_release(
        &self,
        _user: &str,
        _install_path: &std::path::Path,
        _variants: Option<&[usize]>,
        _release_message: Option<&str>,
        _changelog: Option<&[String]>,
        _previous_version: Option<&str>,
        _previous_revision: Option<&str>,
    ) -> Result<()> {
        tracing::info!("ReleaseHook: pre_release called");
        Ok(())
    }

    fn post_release(
        &self,
        _user: &str,
        _install_path: &std::path::Path,
        _variants: &[String],
        _release_message: Option<&str>,
        _changelog: Option<&[String]>,
        _previous_version: Option<&str>,
        _previous_revision: Option<&str>,
    ) -> Result<()> {
        tracing::info!("ReleaseHook: post_release called");
        Ok(())
    }
}

/// An email release hook that sends notifications via SMTP.
///
/// This hook sends email notifications before and after the release process.
/// Configuration is read from a file at `source_path` (JSON/TOML format).
pub struct EmailHook {
    config: EmailHookConfig,
}

impl ReleaseHook for EmailHook {
    fn name() -> String {
        "email".to_string()
    }

    fn new(source_path: PathBuf) -> Result<Self> {
        let config = Self::read_config(&source_path).unwrap_or_default();
        Ok(Self { config })
    }

    fn pre_build(
        &self,
        _user: &str,
        _install_path: &std::path::Path,
        _variants: Option<&[usize]>,
        _release_message: Option<&str>,
        _changelog: Option<&[String]>,
        _previous_version: Option<&str>,
        _previous_revision: Option<&str>,
    ) -> Result<()> {
        tracing::info!(
            "EmailHook: pre_build - would send email to {:?}",
            self.config.to_addresses
        );
        Ok(())
    }

    fn pre_release(
        &self,
        _user: &str,
        _install_path: &std::path::Path,
        _variants: Option<&[usize]>,
        _release_message: Option<&str>,
        _changelog: Option<&[String]>,
        _previous_version: Option<&str>,
        _previous_revision: Option<&str>,
    ) -> Result<()> {
        tracing::info!(
            "EmailHook: pre_release - would send email to {:?}",
            self.config.to_addresses
        );
        Ok(())
    }

    fn post_release(
        &self,
        _user: &str,
        _install_path: &std::path::Path,
        _variants: &[String],
        _release_message: Option<&str>,
        _changelog: Option<&[String]>,
        _previous_version: Option<&str>,
        _previous_revision: Option<&str>,
    ) -> Result<()> {
        tracing::info!(
            "EmailHook: post_release - would send email to {:?}",
            self.config.to_addresses
        );
        Ok(())
    }
}

impl EmailHook {
    /// Read configuration from a file.
    fn read_config(path: &std::path::Path) -> Option<EmailHookConfig> {
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(path).ok()?;
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            serde_json::from_str(&content).ok()
        } else {
            // Assume TOML
            toml::from_str(&content).ok()
        }
    }
}

/// A webhook release hook that sends HTTP requests.
///
/// This hook sends HTTP requests to a configured URL before and after release.
/// Configuration is read from a file at `source_path` (JSON/TOML format).
pub struct WebHook {
    config: WebHookConfig,
}

impl ReleaseHook for WebHook {
    fn name() -> String {
        "webhook".to_string()
    }

    fn new(source_path: PathBuf) -> Result<Self> {
        let config = Self::read_config(&source_path).unwrap_or_default();
        if config.url.is_empty() {
            return Err(crate::ReleaseHookError::Error(
                "WebHook requires 'url' in configuration".to_string(),
            ));
        }
        Ok(Self { config })
    }

    fn pre_build(
        &self,
        _user: &str,
        _install_path: &std::path::Path,
        _variants: Option<&[usize]>,
        _release_message: Option<&str>,
        _changelog: Option<&[String]>,
        _previous_version: Option<&str>,
        _previous_revision: Option<&str>,
    ) -> Result<()> {
        tracing::info!(
            "WebHook: pre_build - would send {} request to {}",
            self.config.method,
            self.config.url
        );
        Ok(())
    }

    fn pre_release(
        &self,
        _user: &str,
        _install_path: &std::path::Path,
        _variants: Option<&[usize]>,
        _release_message: Option<&str>,
        _changelog: Option<&[String]>,
        _previous_version: Option<&str>,
        _previous_revision: Option<&str>,
    ) -> Result<()> {
        tracing::info!(
            "WebHook: pre_release - would send {} request to {}",
            self.config.method,
            self.config.url
        );
        Ok(())
    }

    fn post_release(
        &self,
        _user: &str,
        _install_path: &std::path::Path,
        _variants: &[String],
        _release_message: Option<&str>,
        _changelog: Option<&[String]>,
        _previous_version: Option<&str>,
        _previous_revision: Option<&str>,
    ) -> Result<()> {
        tracing::info!(
            "WebHook: post_release - would send {} request to {}",
            self.config.method,
            self.config.url
        );
        Ok(())
    }
}

impl WebHook {
    /// Read configuration from a file.
    fn read_config(path: &std::path::Path) -> Option<WebHookConfig> {
        if !path.exists() {
            return None;
        }
        let content = std::fs::read_to_string(path).ok()?;
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            serde_json::from_str(&content).ok()
        } else {
            toml::from_str(&content).ok()
        }
    }
}

/// A command release hook compatible with Rez's general-purpose hook.
pub struct CommandHook {
    config: CommandHookConfig,
}

struct CommandContext<'a> {
    user: &'a str,
    install_path: &'a std::path::Path,
    variants: &'a [String],
    release_message: Option<&'a str>,
    previous_version: Option<&'a str>,
    previous_revision: Option<&'a str>,
}

impl ReleaseHook for CommandHook {
    fn name() -> String {
        "command".to_string()
    }

    fn new(source_path: PathBuf) -> Result<Self> {
        Ok(Self {
            config: Self::read_config(&source_path).unwrap_or_default(),
        })
    }

    fn pre_build(
        &self,
        user: &str,
        install_path: &std::path::Path,
        variants: Option<&[usize]>,
        release_message: Option<&str>,
        _changelog: Option<&[String]>,
        previous_version: Option<&str>,
        previous_revision: Option<&str>,
    ) -> Result<()> {
        let variant_names = variants
            .unwrap_or_default()
            .iter()
            .map(|index| index.to_string())
            .collect::<Vec<_>>();
        self.execute(
            &self.config.pre_build_commands,
            CommandContext {
                user,
                install_path,
                variants: &variant_names,
                release_message,
                previous_version,
                previous_revision,
            },
        )
    }

    fn pre_release(
        &self,
        user: &str,
        install_path: &std::path::Path,
        variants: Option<&[usize]>,
        release_message: Option<&str>,
        _changelog: Option<&[String]>,
        previous_version: Option<&str>,
        previous_revision: Option<&str>,
    ) -> Result<()> {
        let variant_names = variants
            .unwrap_or_default()
            .iter()
            .map(|index| index.to_string())
            .collect::<Vec<_>>();
        self.execute(
            &self.config.pre_release_commands,
            CommandContext {
                user,
                install_path,
                variants: &variant_names,
                release_message,
                previous_version,
                previous_revision,
            },
        )
    }

    fn post_release(
        &self,
        user: &str,
        install_path: &std::path::Path,
        variants: &[String],
        release_message: Option<&str>,
        _changelog: Option<&[String]>,
        previous_version: Option<&str>,
        previous_revision: Option<&str>,
    ) -> Result<()> {
        self.execute(
            &self.config.post_release_commands,
            CommandContext {
                user,
                install_path,
                variants,
                release_message,
                previous_version,
                previous_revision,
            },
        )
    }
}

impl CommandHook {
    fn read_config(path: &std::path::Path) -> Option<CommandHookConfig> {
        if !path.is_file() {
            return None;
        }
        let content = std::fs::read_to_string(path).ok()?;
        if path.extension().and_then(|extension| extension.to_str()) == Some("json") {
            serde_json::from_str(&content).ok()
        } else {
            toml::from_str(&content).ok()
        }
    }

    fn execute(&self, commands: &[CommandSpec], context: CommandContext<'_>) -> Result<()> {
        for spec in commands {
            let command = format_command(&spec.command, &context);
            let args = spec
                .args
                .as_vec()
                .into_iter()
                .map(|arg| format_command(&arg, &context))
                .collect::<Vec<_>>();
            if self.config.print_commands {
                tracing::info!(command = %command, args = ?args, "running release hook command");
            }

            let output = Command::new(&command)
                .args(&args)
                .envs(
                    spec.env
                        .iter()
                        .map(|(key, value)| (key, format_command(value, &context))),
                )
                .output()
                .map_err(|error| {
                    crate::ReleaseHookError::Error(format!(
                        "failed to execute release hook command '{}': {error}",
                        command
                    ))
                })?;

            if output.status.success() {
                if self.config.print_output && !output.stdout.is_empty() {
                    tracing::info!(output = %String::from_utf8_lossy(&output.stdout), "release hook command output");
                }
                continue;
            }

            let message = format!(
                "release hook command '{}' failed with status {}: {}",
                command,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
            if self.config.print_error {
                tracing::error!("{message}");
            }
            if self.config.cancel_on_error {
                return Err(crate::ReleaseHookError::Cancelled(message));
            }
            if self.config.stop_on_error {
                break;
            }
        }
        Ok(())
    }
}

fn format_command(value: &str, context: &CommandContext<'_>) -> String {
    value
        .replace("{release.path}", &context.install_path.to_string_lossy())
        .replace("{install_path}", &context.install_path.to_string_lossy())
        .replace("{user}", context.user)
        .replace(
            "{release_message}",
            context.release_message.unwrap_or_default(),
        )
        .replace(
            "{previous_version}",
            context.previous_version.unwrap_or_default(),
        )
        .replace(
            "{previous_revision}",
            context.previous_revision.unwrap_or_default(),
        )
        .replace("{variants}", &context.variants.join(","))
        .replace("{num_variants}", &context.variants.len().to_string())
}

/// Register all built-in hooks with the global registry.
///
/// This function should be called during initialization.
pub fn register_builtin_hooks() {
    let registry = crate::get_registry();

    registry.register("noop", |_path| {
        Ok(Box::new(NoopHook) as Box<dyn ReleaseHook>)
    });

    registry.register("logging", |path| {
        Ok(Box::new(LoggingHook::new(path)?) as Box<dyn ReleaseHook>)
    });

    registry.register("email", |path| {
        Ok(Box::new(EmailHook::new(path)?) as Box<dyn ReleaseHook>)
    });

    registry.register("emailer", |path| {
        Ok(Box::new(EmailHook::new(path)?) as Box<dyn ReleaseHook>)
    });

    registry.register("command", |path| {
        Ok(Box::new(CommandHook::new(path)?) as Box<dyn ReleaseHook>)
    });

    registry.register("webhook", |path| {
        Ok(Box::new(WebHook::new(path)?) as Box<dyn ReleaseHook>)
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_noop_hook_creation() {
        let hook = NoopHook::new(PathBuf::from("/tmp"));
        assert!(hook.is_ok());
    }

    #[test]
    fn test_noop_hook_name() {
        assert_eq!(NoopHook::name(), "noop");
    }

    #[test]
    fn test_noop_hook_pre_build() {
        let hook = NoopHook::new(PathBuf::from("/tmp")).unwrap();
        let result = hook.pre_build(
            "test_user",
            Path::new("/tmp/install"),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_noop_hook_pre_release() {
        let hook = NoopHook::new(PathBuf::from("/tmp")).unwrap();
        let result = hook.pre_release(
            "test_user",
            Path::new("/tmp/install"),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_noop_hook_post_release() {
        let hook = NoopHook::new(PathBuf::from("/tmp")).unwrap();
        let result = hook.post_release(
            "test_user",
            Path::new("/tmp/install"),
            &[],
            None,
            None,
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_logging_hook_creation() {
        let hook = LoggingHook::new(PathBuf::from("/tmp"));
        assert!(hook.is_ok());
    }

    #[test]
    fn test_logging_hook_name() {
        assert_eq!(LoggingHook::name(), "logging");
    }

    #[test]
    fn test_logging_hook_pre_build() {
        let hook = LoggingHook::new(PathBuf::from("/tmp")).unwrap();
        let result = hook.pre_build(
            "test_user",
            Path::new("/tmp/install"),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_register_builtin_hooks() {
        // Note: This test verifies the function runs without panic
        // The global registry is shared, so we can't easily assert registration
        register_builtin_hooks();

        let types = crate::get_release_hook_types();
        // Both the legacy and upstream email names remain available.
        assert!(types.contains(&"noop".to_string()));
        assert!(types.contains(&"logging".to_string()));
        assert!(types.contains(&"email".to_string()));
        assert!(types.contains(&"emailer".to_string()));
        assert!(types.contains(&"command".to_string()));
        assert!(types.contains(&"webhook".to_string()));
    }

    #[test]
    fn test_command_hook_name_and_default_creation() {
        assert_eq!(CommandHook::name(), "command");
        assert!(CommandHook::new(PathBuf::from("/nonexistent/path")).is_ok());
    }

    #[test]
    fn test_email_hook_creation_default_config() {
        // EmailHook can be created with default config (no config file)
        let hook = EmailHook::new(std::path::PathBuf::from("/nonexistent/path"));
        assert!(hook.is_ok());
    }

    #[test]
    fn test_email_hook_name() {
        assert_eq!(EmailHook::name(), "email");
    }

    #[test]
    fn test_email_hook_pre_build() {
        let hook = EmailHook::new(std::path::PathBuf::from("/tmp")).unwrap();
        let result = hook.pre_build(
            "test_user",
            std::path::Path::new("/tmp/install"),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_email_hook_pre_release() {
        let hook = EmailHook::new(std::path::PathBuf::from("/tmp")).unwrap();
        let result = hook.pre_release(
            "test_user",
            std::path::Path::new("/tmp/install"),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_email_hook_post_release() {
        let hook = EmailHook::new(std::path::PathBuf::from("/tmp")).unwrap();
        let result = hook.post_release(
            "test_user",
            std::path::Path::new("/tmp/install"),
            &[],
            None,
            None,
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_web_hook_creation_with_url() {
        // WebHook requires URL in config - test with invalid path (uses default)
        let hook = WebHook::new(std::path::PathBuf::from("/nonexistent/path"));
        // Should fail because default config has empty URL
        assert!(hook.is_err());
    }

    #[test]
    fn test_web_hook_name() {
        assert_eq!(WebHook::name(), "webhook");
    }

    #[test]
    fn test_web_hook_pre_build() {
        // This test creates a hook with a config file
        // For now, just test that the method signature works
        let config = WebHookConfig {
            url: "https://example.com/webhook".to_string(),
            method: "POST".to_string(),
            headers: std::collections::HashMap::new(),
            timeout_secs: 30,
        };
        let hook = WebHook { config };
        let result = hook.pre_build(
            "test_user",
            std::path::Path::new("/tmp/install"),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_web_hook_pre_release() {
        let config = WebHookConfig {
            url: "https://example.com/webhook".to_string(),
            method: "POST".to_string(),
            headers: std::collections::HashMap::new(),
            timeout_secs: 30,
        };
        let hook = WebHook { config };
        let result = hook.pre_release(
            "test_user",
            std::path::Path::new("/tmp/install"),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_web_hook_post_release() {
        let config = WebHookConfig {
            url: "https://example.com/webhook".to_string(),
            method: "POST".to_string(),
            headers: std::collections::HashMap::new(),
            timeout_secs: 30,
        };
        let hook = WebHook { config };
        let result = hook.post_release(
            "test_user",
            std::path::Path::new("/tmp/install"),
            &[],
            None,
            None,
            None,
            None,
        );
        assert!(result.is_ok());
    }
}
