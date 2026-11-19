//! Build process management

use crate::{
    BuildArtifacts, BuildConfig, BuildEnvironment, BuildRequest, BuildResult, BuildStatus,
    BuildSystem,
};
use rez_next_common::RezCoreError;
use rez_next_context::{ContextConfig, EnvironmentManager, ShellExecutor, ShellType};
use rez_next_package::serialization::{PackageFormat, PackageSerializer};
use rez_next_package::{Package, TestCommand, TestVariantFilter};
use rez_next_rex::{RexEnvironment, RexExecutor};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::process::Child;
use tokio::sync::{Mutex, RwLock, mpsc};

/// Build process for managing individual package builds
#[derive(Debug)]
pub struct BuildProcess {
    /// Build ID
    build_id: String,
    /// Build request
    request: BuildRequest,
    /// Build environment
    environment: BuildEnvironment,
    /// Build configuration
    config: BuildConfig,
    /// Build status
    status: Arc<RwLock<BuildStatus>>,
    /// Build output
    output: Arc<Mutex<String>>,
    /// Build errors
    errors: Arc<Mutex<String>>,
    /// Start time
    start_time: Option<std::time::Instant>,
    /// Child process (if running)
    child_process: Arc<Mutex<Option<Child>>>,
}

/// Build step enumeration
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BuildStep {
    /// Preparing build environment
    Preparing,
    /// Configuring build
    Configuring,
    /// Compiling source code
    Compiling,
    /// Running tests
    Testing,
    /// Packaging artifacts
    Packaging,
    /// Installing artifacts
    Installing,
    /// Running tests declared by the installed package
    PackageTesting,
    /// Cleaning up
    Cleanup,
}

/// Live build event kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BuildEventKind {
    /// Build process started.
    BuildStarted,
    /// One build step started.
    StepStarted,
    /// One line or chunk of step output.
    StepOutput,
    /// One line or chunk of step error output.
    StepError,
    /// One build step finished.
    StepFinished,
    /// Build process finished.
    BuildFinished,
}

/// Live build event emitted while a build is running.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildEvent {
    /// Build ID.
    pub build_id: String,
    /// Build step, if the event belongs to a step.
    pub step: Option<BuildStep>,
    /// Event kind.
    pub kind: BuildEventKind,
    /// Human-readable event message.
    pub message: String,
    /// Whether the event represents a successful result.
    pub success: Option<bool>,
    /// Event duration in milliseconds, when relevant.
    pub duration_ms: Option<u64>,
    /// Milliseconds since Unix epoch.
    pub timestamp_ms: u128,
}

/// Build step result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildStepResult {
    /// Build step
    pub step: BuildStep,
    /// Whether step was successful
    pub success: bool,
    /// Step output
    pub output: String,
    /// Step errors
    pub errors: String,
    /// Step duration in milliseconds
    pub duration_ms: u64,
}

impl BuildProcess {
    /// Create a new build process
    pub fn new(
        build_id: String,
        request: BuildRequest,
        environment: BuildEnvironment,
        config: BuildConfig,
    ) -> Self {
        Self {
            build_id,
            request,
            environment,
            config,
            status: Arc::new(RwLock::new(BuildStatus::Queued)),
            output: Arc::new(Mutex::new(String::new())),
            errors: Arc::new(Mutex::new(String::new())),
            start_time: None,
            child_process: Arc::new(Mutex::new(None)),
        }
    }

    /// Start the build process
    pub async fn start(&mut self) -> Result<(), RezCoreError> {
        {
            let mut status = self.status.write().await;
            *status = BuildStatus::Running;
        }

        self.start_time = Some(std::time::Instant::now());

        // Start build in background
        let build_id = self.build_id.clone();
        let request = self.request.clone();
        let mut environment = self.environment.clone();
        let config = self.config.clone();
        let status = self.status.clone();
        let output = self.output.clone();
        let errors = self.errors.clone();
        let child_process = self.child_process.clone();

        tokio::spawn(async move {
            let result = Self::run_build_steps(
                &build_id,
                &request,
                &mut environment,
                &config,
                output.clone(),
                errors.clone(),
                child_process.clone(),
            )
            .await;

            if let Err(error) = &result {
                let mut errors_guard = errors.lock().await;
                if errors_guard.trim().is_empty() {
                    errors_guard.push_str(&error.to_string());
                }
            }

            let mut status_guard = status.write().await;
            *status_guard = match result {
                Ok(_) => BuildStatus::Success,
                Err(_) => BuildStatus::Failed,
            };
        });

        Ok(())
    }

    /// Wait for the build to complete
    pub async fn wait(&mut self) -> Result<BuildResult, RezCoreError> {
        // Wait for build to complete
        loop {
            let status = {
                let status_guard = self.status.read().await;
                status_guard.clone()
            };

            match status {
                BuildStatus::Success | BuildStatus::Failed | BuildStatus::Cancelled => break,
                _ => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
            }
        }

        // Collect results
        let duration_ms = self
            .start_time
            .map(|start| start.elapsed().as_millis() as u64)
            .unwrap_or(0);

        let output = {
            let output_guard = self.output.lock().await;
            output_guard.clone()
        };

        let errors = {
            let errors_guard = self.errors.lock().await;
            errors_guard.clone()
        };

        let status = {
            let status_guard = self.status.read().await;
            status_guard.clone()
        };

        let success = status == BuildStatus::Success;

        // Create artifacts (simplified)
        let artifacts = if success {
            BuildArtifacts::new(self.environment.get_install_dir().clone())
        } else {
            BuildArtifacts::default()
        };

        let result = if success {
            BuildResult::success(self.build_id.clone(), artifacts, duration_ms)
        } else {
            BuildResult::failure(self.build_id.clone(), errors, duration_ms)
        };

        Ok(result.with_output(output))
    }

    /// Cancel the build process
    pub async fn cancel(&mut self) -> Result<(), RezCoreError> {
        {
            let mut status = self.status.write().await;
            *status = BuildStatus::Cancelled;
        }

        // Kill child process if running
        let mut child_guard = self.child_process.lock().await;
        if let Some(ref mut child) = child_guard.as_mut() {
            let _ = child.kill().await;
        }

        Ok(())
    }

    /// Get current build status
    pub fn get_status(&self) -> BuildStatus {
        // Use try_read to avoid blocking
        match self.status.try_read() {
            Ok(status) => status.clone(),
            Err(_) => BuildStatus::Running, // If we can't read, assume still running
        }
    }

    /// Run build steps
    async fn run_build_steps(
        build_id: &str,
        request: &BuildRequest,
        environment: &mut BuildEnvironment,
        config: &BuildConfig,
        output: Arc<Mutex<String>>,
        errors: Arc<Mutex<String>>,
        child_process: Arc<Mutex<Option<Child>>>,
    ) -> Result<(), RezCoreError> {
        if config.clean_before_build || request.options.force_rebuild {
            environment.clean().await?;
        }

        let event_sender = config.event_sender.clone();
        emit_build_event(
            event_sender.as_ref(),
            build_id,
            None,
            BuildEventKind::BuildStarted,
            format!(
                "Building {}",
                request
                    .package
                    .version
                    .as_ref()
                    .map(|version| format!("{}-{}", request.package.name, version.as_str()))
                    .unwrap_or_else(|| request.package.name.clone())
            ),
            None,
            None,
        );

        let steps = vec![
            BuildStep::Preparing,
            BuildStep::Configuring,
            BuildStep::Compiling,
            BuildStep::Testing,
            BuildStep::Packaging,
            BuildStep::Installing,
            BuildStep::PackageTesting,
            BuildStep::Cleanup,
        ];

        for step in steps {
            emit_build_event(
                event_sender.as_ref(),
                build_id,
                Some(step.clone()),
                BuildEventKind::StepStarted,
                format!("{:?} started", step),
                None,
                None,
            );

            let step_result = Self::execute_build_step(
                &step,
                request,
                environment,
                config,
                child_process.clone(),
            )
            .await?;

            emit_step_lines(
                event_sender.as_ref(),
                build_id,
                &step,
                BuildEventKind::StepOutput,
                &step_result.output,
            );

            // Append output
            {
                let mut output_guard = output.lock().await;
                output_guard.push_str(&format!("=== {:?} ===\n", step));
                output_guard.push_str(&step_result.output);
                output_guard.push('\n');
            }

            // Append errors if any
            if !step_result.errors.is_empty() {
                emit_step_lines(
                    event_sender.as_ref(),
                    build_id,
                    &step,
                    BuildEventKind::StepError,
                    &step_result.errors,
                );

                let mut errors_guard = errors.lock().await;
                errors_guard.push_str(&format!("=== {:?} Errors ===\n", step));
                errors_guard.push_str(&step_result.errors);
                errors_guard.push('\n');
            }

            emit_build_event(
                event_sender.as_ref(),
                build_id,
                Some(step.clone()),
                BuildEventKind::StepFinished,
                format!("{:?} finished", step),
                Some(step_result.success),
                Some(step_result.duration_ms),
            );

            // Stop on failure
            if !step_result.success {
                emit_build_event(
                    event_sender.as_ref(),
                    build_id,
                    None,
                    BuildEventKind::BuildFinished,
                    format!("Build failed during {:?}", step),
                    Some(false),
                    None,
                );
                return Err(RezCoreError::BuildError(format!(
                    "Build step {:?} failed: {}",
                    step, step_result.errors
                )));
            }
        }

        emit_build_event(
            event_sender.as_ref(),
            build_id,
            None,
            BuildEventKind::BuildFinished,
            "Build finished".to_string(),
            Some(true),
            None,
        );

        Ok(())
    }

    /// Execute a single build step
    async fn execute_build_step(
        step: &BuildStep,
        request: &BuildRequest,
        environment: &mut BuildEnvironment,
        config: &BuildConfig,
        child_process: Arc<Mutex<Option<Child>>>,
    ) -> Result<BuildStepResult, RezCoreError> {
        let start_time = std::time::Instant::now();

        let (success, output, errors) = match step {
            BuildStep::Preparing => {
                Self::execute_prepare_step(request, environment, config).await?
            }
            BuildStep::Configuring => {
                Self::execute_configure_step(request, environment, config).await?
            }
            BuildStep::Compiling => {
                Self::execute_compile_step(request, environment, config, child_process).await?
            }
            BuildStep::Testing => {
                if request.options.skip_tests {
                    (true, "Tests skipped".to_string(), String::new())
                } else {
                    Self::execute_test_step(request, environment, config, child_process).await?
                }
            }
            BuildStep::Packaging => {
                Self::execute_package_step(request, environment, config).await?
            }
            BuildStep::Installing => {
                Self::execute_install_step(request, environment, config).await?
            }
            BuildStep::PackageTesting => {
                if request.options.skip_tests {
                    (true, "Package tests skipped".to_string(), String::new())
                } else {
                    Self::execute_package_test_step(request, environment).await?
                }
            }
            BuildStep::Cleanup => Self::execute_cleanup_step(request, environment, config).await?,
        };

        let duration_ms = start_time.elapsed().as_millis() as u64;

        Ok(BuildStepResult {
            step: step.clone(),
            success,
            output,
            errors,
            duration_ms,
        })
    }

    /// Execute prepare step
    async fn execute_prepare_step(
        _request: &BuildRequest,
        environment: &BuildEnvironment,
        _config: &BuildConfig,
    ) -> Result<(bool, String, String), RezCoreError> {
        // Create build directories
        let build_dir = environment.get_build_dir();
        let install_dir = environment.get_install_dir();

        tokio::fs::create_dir_all(build_dir)
            .await
            .map_err(|e| RezCoreError::BuildError(format!("Failed to create build dir: {}", e)))?;

        tokio::fs::create_dir_all(install_dir).await.map_err(|e| {
            RezCoreError::BuildError(format!("Failed to create install dir: {}", e))
        })?;

        let output = format!(
            "Created build directory: {}\nCreated install directory: {}",
            build_dir.display(),
            install_dir.display()
        );

        Ok((true, output, String::new()))
    }

    /// Execute configure step
    async fn execute_configure_step(
        request: &BuildRequest,
        environment: &mut BuildEnvironment,
        _config: &BuildConfig,
    ) -> Result<(bool, String, String), RezCoreError> {
        apply_pre_build_commands(request, environment)?;
        // Detect and configure build system
        let build_system = BuildSystem::detect_with_package(&request.source_dir, &request.package)?;
        let configure_result = build_system.configure(request, environment).await?;

        Ok((
            configure_result.success,
            configure_result.output,
            configure_result.errors,
        ))
    }

    /// Execute compile step
    async fn execute_compile_step(
        request: &BuildRequest,
        environment: &BuildEnvironment,
        _config: &BuildConfig,
        child_process: Arc<Mutex<Option<Child>>>,
    ) -> Result<(bool, String, String), RezCoreError> {
        let build_system = BuildSystem::detect_with_package(&request.source_dir, &request.package)?;
        let compile_result = build_system
            .compile(request, environment, child_process)
            .await?;

        Ok((
            compile_result.success,
            compile_result.output,
            compile_result.errors,
        ))
    }

    /// Execute test step
    async fn execute_test_step(
        request: &BuildRequest,
        environment: &BuildEnvironment,
        _config: &BuildConfig,
        child_process: Arc<Mutex<Option<Child>>>,
    ) -> Result<(bool, String, String), RezCoreError> {
        let build_system = BuildSystem::detect_with_package(&request.source_dir, &request.package)?;
        let test_result = build_system
            .test(request, environment, child_process)
            .await?;

        Ok((test_result.success, test_result.output, test_result.errors))
    }

    /// Execute package step
    async fn execute_package_step(
        request: &BuildRequest,
        environment: &BuildEnvironment,
        _config: &BuildConfig,
    ) -> Result<(bool, String, String), RezCoreError> {
        let build_system = BuildSystem::detect_with_package(&request.source_dir, &request.package)?;
        let package_result = build_system.package(request, environment).await?;

        Ok((
            package_result.success,
            package_result.output,
            package_result.errors,
        ))
    }

    /// Execute install step
    async fn execute_install_step(
        request: &BuildRequest,
        environment: &BuildEnvironment,
        _config: &BuildConfig,
    ) -> Result<(bool, String, String), RezCoreError> {
        let build_system = BuildSystem::detect_with_package(&request.source_dir, &request.package)?;
        let install_result = build_system.install(request, environment).await?;

        Ok((
            install_result.success,
            install_result.output,
            install_result.errors,
        ))
    }

    async fn install_package_definition(
        request: &BuildRequest,
        environment: &BuildEnvironment,
    ) -> Result<Option<PathBuf>, RezCoreError> {
        let Some((source, destination)) = Self::package_definition_paths(request, environment)
        else {
            return Ok(None);
        };
        if source == destination {
            return Ok(None);
        }

        let timestamp = package_timestamp(&request.package);
        if source
            .extension()
            .is_some_and(|extension| extension == "py")
        {
            let content = tokio::fs::read_to_string(&source).await.map_err(|error| {
                RezCoreError::BuildError(format!(
                    "Failed to read package definition {}: {}",
                    source.display(),
                    error
                ))
            })?;
            let content = if request.package.timestamp.is_some() {
                content
            } else {
                format!(
                    "{content}\n\n# Package metadata written by rez-next\ntimestamp = {timestamp}\n"
                )
            };
            tokio::fs::write(&destination, content)
                .await
                .map_err(|error| {
                    RezCoreError::BuildError(format!(
                        "Failed to install package definition {}: {}",
                        destination.display(),
                        error
                    ))
                })?;
        } else {
            let mut package = request.package.clone();
            package.timestamp = Some(timestamp);
            PackageSerializer::save_to_file(&package, &destination, PackageFormat::Yaml).map_err(
                |error| {
                    RezCoreError::BuildError(format!(
                        "Failed to install package definition {}: {}",
                        destination.display(),
                        error
                    ))
                },
            )?;
        }
        Ok(Some(destination))
    }

    fn package_definition_paths(
        request: &BuildRequest,
        environment: &BuildEnvironment,
    ) -> Option<(PathBuf, PathBuf)> {
        let source = request
            .package
            .filepath
            .as_deref()
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .or_else(|| {
                ["package.py", "package.yaml", "package.yml"]
                    .iter()
                    .map(|name| request.source_dir.join(name))
                    .find(|path| path.is_file())
            })?;
        let file_name = source.file_name()?.to_owned();
        Some((source, environment.get_install_dir().join(file_name)))
    }

    async fn execute_package_test_step(
        request: &BuildRequest,
        environment: &BuildEnvironment,
    ) -> Result<(bool, String, String), RezCoreError> {
        let install_dir = environment.get_install_dir();
        let definition_destination = Self::package_definition_paths(request, environment)
            .map(|(_, destination)| destination);
        if request.package.tests.is_empty() {
            let output = match Self::install_package_definition(request, environment).await? {
                Some(definition) => format!(
                    "No package tests defined\nInstalled package definition: {}",
                    definition.display()
                ),
                None => "No package tests defined".to_string(),
            };
            return Ok((true, output, String::new()));
        }

        let mut base_environment = std::collections::HashMap::new();
        for (name, value) in std::env::vars().chain(environment.get_env_vars().clone()) {
            let name = if cfg!(windows) {
                name.to_ascii_uppercase()
            } else {
                name
            };
            base_environment.insert(name, value);
        }
        let manager = EnvironmentManager::with_base_environment(
            ContextConfig {
                inherit_parent_env: false,
                ..ContextConfig::default()
            },
            base_environment,
        );
        let mut tests: Vec<_> = request.package.tests.iter().collect();
        tests.sort_by_key(|(name, _)| *name);
        let mut output = String::new();
        for (name, definition) in tests {
            if !package_test_is_selected(request, definition) {
                output.push_str(&format!("Skipping package test '{name}'\n"));
                continue;
            }

            let mut package = request.package.clone();
            package.filepath = definition_destination
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned());
            if let Some(pre_test_commands) = package.pre_test_commands.take() {
                package.post_commands = Some(match package.post_commands.take() {
                    Some(post_commands) => format!("{post_commands}\n{pre_test_commands}"),
                    None => pre_test_commands,
                });
            }
            package.requires.extend(definition.requires.iter().cloned());
            let test_environment = match manager.generate_environment(&[package]).await {
                Ok(environment) => environment,
                Err(error) => {
                    let rollback_error = Self::rollback_install_dir(install_dir).await;
                    return Ok((
                        false,
                        output,
                        append_rollback_error(error.to_string(), rollback_error),
                    ));
                }
            };
            let executor = ShellExecutor::with_shell(ShellType::detect())
                .with_environment(test_environment.clone())
                .with_working_directory(request.source_dir.clone());
            let command = definition.command.expanded_for_root(install_dir);
            let result = match command {
                TestCommand::String(command) => {
                    output.push_str(&format!("Running package test '{name}': {command}\n"));
                    executor.execute(&command).await
                }
                TestCommand::List(arguments) => {
                    let Some(program) = arguments.first() else {
                        let errors = format!("Package test '{name}' has an empty argv command");
                        let rollback_error = Self::rollback_install_dir(install_dir).await;
                        return Ok((false, output, append_rollback_error(errors, rollback_error)));
                    };
                    output.push_str(&format!(
                        "Running package test '{name}': {}\n",
                        arguments.join(" ")
                    ));
                    let started_at = Instant::now();
                    let result = tokio::process::Command::new(program)
                        .args(&arguments[1..])
                        .current_dir(&request.source_dir)
                        .env_clear()
                        .envs(&test_environment)
                        .output()
                        .await
                        .map_err(|error| {
                            RezCoreError::ExecutionError(format!(
                                "Failed to execute package test '{name}': {error}"
                            ))
                        });
                    result.map(|result| rez_next_context::CommandResult {
                        exit_code: result.status.code().unwrap_or(-1),
                        stdout: String::from_utf8_lossy(&result.stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
                        execution_time_ms: started_at.elapsed().as_millis() as u64,
                    })
                }
            };
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    let rollback_error = Self::rollback_install_dir(install_dir).await;
                    return Ok((
                        false,
                        output,
                        append_rollback_error(error.to_string(), rollback_error),
                    ));
                }
            };
            output.push_str(&result.stdout);
            if !result.is_success() {
                let errors = if result.stderr.trim().is_empty() {
                    format!(
                        "Package test '{name}' exited with code {}",
                        result.exit_code
                    )
                } else {
                    result.stderr
                };
                let rollback_error = Self::rollback_install_dir(install_dir).await;
                return Ok((false, output, append_rollback_error(errors, rollback_error)));
            }
        }

        match Self::install_package_definition(request, environment).await {
            Ok(Some(definition)) => {
                output.push_str(&format!(
                    "Installed package definition: {}\n",
                    definition.display()
                ));
                Ok((true, output, String::new()))
            }
            Ok(None) => Ok((true, output, String::new())),
            Err(error) => {
                let rollback_error = Self::rollback_install_dir(install_dir).await;
                Ok((
                    false,
                    output,
                    append_rollback_error(error.to_string(), rollback_error),
                ))
            }
        }
    }

    async fn rollback_install_dir(install_dir: &std::path::Path) -> Option<String> {
        if !install_dir.exists() {
            return None;
        }
        tokio::fs::remove_dir_all(install_dir)
            .await
            .err()
            .map(|error| format!("Failed to roll back installed payload: {error}"))
    }

    /// Execute cleanup step
    async fn execute_cleanup_step(
        _request: &BuildRequest,
        environment: &BuildEnvironment,
        config: &BuildConfig,
    ) -> Result<(bool, String, String), RezCoreError> {
        // Clean temporary files if configured
        if !config.keep_artifacts {
            let temp_dir = environment.get_temp_dir();
            if temp_dir.exists() {
                tokio::fs::remove_dir_all(temp_dir).await.map_err(|e| {
                    RezCoreError::BuildError(format!("Failed to clean temp dir: {}", e))
                })?;
            }
        }

        Ok((true, "Cleanup completed".to_string(), String::new()))
    }
}

fn apply_pre_build_commands(
    request: &BuildRequest,
    environment: &mut BuildEnvironment,
) -> Result<(), RezCoreError> {
    let Some(commands) = request
        .package
        .pre_build_commands
        .as_deref()
        .filter(|commands| !commands.trim().is_empty())
    else {
        return Ok(());
    };

    let root = request.source_dir.to_string_lossy();
    let build_path = environment.get_build_dir().to_string_lossy();
    let install_path = environment.get_install_dir().to_string_lossy();
    let build_type = environment
        .get_env_vars()
        .get("REZ_BUILD_TYPE")
        .map(String::as_str)
        .unwrap_or("local");
    let install = environment
        .get_env_vars()
        .get("REZ_BUILD_INSTALL")
        .map(|value| value == "1")
        .unwrap_or(false);

    let mut executor = RexExecutor::new();
    executor.set_context_var("this.root", root.as_ref());
    executor.set_context_var("this.name", &request.package.name);
    if let Some(version) = request.package.version.as_ref() {
        executor.set_context_var("this.version", version.as_str());
    }
    executor.set_context_var("build.type", build_type);
    executor.set_context_var("build.install", install.to_string());
    executor.set_context_var("build.path", build_path.as_ref());
    executor.set_context_var("build.install_path", install_path.as_ref());
    executor.execute_commands(
        commands,
        &request.package.name,
        Some(root.as_ref()),
        request
            .package
            .version
            .as_ref()
            .map(|version| version.as_str()),
    )?;

    let mut resolved = RexEnvironment::new();
    resolved.vars = environment.get_env_vars().clone();
    resolved.apply(executor.get_actions());

    let existing_keys = environment
        .get_env_vars()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    for key in existing_keys {
        if !resolved.vars.contains_key(&key) {
            environment.remove_env_var(&key);
        }
    }
    for (key, value) in resolved.vars {
        environment.add_env_var(key, value);
    }
    Ok(())
}

fn package_timestamp(package: &Package) -> i64 {
    package.timestamp.unwrap_or_else(|| {
        std::env::var("SOURCE_DATE_EPOCH")
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .or_else(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|duration| duration.as_secs() as i64)
            })
            .unwrap_or_default()
    })
}

fn package_test_is_selected(
    request: &BuildRequest,
    definition: &rez_next_package::PackageTestDefinition,
) -> bool {
    let run_on = if request.options.release_mode {
        "pre_release"
    } else {
        "default"
    };
    if !definition.run_on.iter().any(|tag| tag == run_on) {
        return false;
    }

    match definition.on_variants.as_ref() {
        Some(TestVariantFilter::Enabled(true)) => true,
        Some(filter @ TestVariantFilter::Requires(_)) => {
            filter.matches_variant_requires(request.variant_requires.as_deref().unwrap_or_default())
        }
        Some(TestVariantFilter::Enabled(false)) | None => {
            request.package.variants.len() <= 1
                || request.variant_index.is_none_or(|index| index == 0)
        }
    }
}

fn append_rollback_error(errors: String, rollback_error: Option<String>) -> String {
    match rollback_error {
        Some(rollback_error) => format!("{errors}\n{rollback_error}"),
        None => errors,
    }
}

fn emit_step_lines(
    sender: Option<&mpsc::UnboundedSender<BuildEvent>>,
    build_id: &str,
    step: &BuildStep,
    kind: BuildEventKind,
    text: &str,
) {
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        emit_build_event(
            sender,
            build_id,
            Some(step.clone()),
            kind.clone(),
            line.trim().to_string(),
            None,
            None,
        );
    }
}

fn emit_build_event(
    sender: Option<&mpsc::UnboundedSender<BuildEvent>>,
    build_id: &str,
    step: Option<BuildStep>,
    kind: BuildEventKind,
    message: String,
    success: Option<bool>,
    duration_ms: Option<u64>,
) {
    let Some(sender) = sender else {
        return;
    };

    let timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let _ = sender.send(BuildEvent {
        build_id: build_id.to_string(),
        step,
        kind,
        message,
        success,
        duration_ms,
        timestamp_ms,
    });
}

#[cfg(test)]
mod tests {
    use super::{BuildEnvironment, BuildRequest, apply_pre_build_commands};
    use rez_next_package::Package;
    use tempfile::tempdir;

    #[test]
    fn pre_build_commands_update_the_build_environment() {
        let temp = tempdir().unwrap();
        let mut package = Package::new("pre-build-package".to_string());
        package.pre_build_commands = Some(
            "env.setenv('PRE_BUILD_ROOT', '{this.root}')\n\
             env.setenv('PRE_BUILD_PATH', '{build.path}')\n\
             env.setenv('PRE_BUILD_TYPE', '{build.type}')"
                .replace("             ", ""),
        );
        let request = BuildRequest::new(package, None, temp.path().to_path_buf());
        let mut environment =
            BuildEnvironment::new(&request.package, &temp.path().join("build"), None).unwrap();

        apply_pre_build_commands(&request, &mut environment).unwrap();

        assert_eq!(
            environment.get_env_vars().get("PRE_BUILD_ROOT"),
            Some(&temp.path().to_string_lossy().into_owned())
        );
        assert_eq!(
            environment.get_env_vars().get("PRE_BUILD_PATH"),
            Some(&environment.get_build_dir().to_string_lossy().into_owned())
        );
        assert_eq!(
            environment.get_env_vars().get("PRE_BUILD_TYPE"),
            Some(&"local".to_string())
        );
    }
}
