use rez_next_context::{ContextConfig, EnvironmentManager, PathStrategy, ResolvedContext};
use rez_next_package::{Package, PackageRequirement, PythonAstParser};
use rez_next_version::Version;
use std::collections::HashMap;
use std::path::PathBuf;

fn package_with_python_path(root: &str) -> Package {
    Package {
        name: "python_tools".to_string(),
        version: Some(Version::parse("1.0.0").expect("valid test version")),
        filepath: Some(
            PathBuf::from(root)
                .join("package.py")
                .to_string_lossy()
                .into_owned(),
        ),
        commands: Some("env.prepend_path('PYTHONPATH', '{root}/python')".to_string()),
        ..Default::default()
    }
}

#[test]
fn selected_parent_variable_is_inherited_without_leaking_unselected_variables() {
    let package_root = PathBuf::from("bundle").join("python_tools").join("1.0.0");
    let package = package_with_python_path(&package_root.to_string_lossy());
    let parent_python_path = PathBuf::from("parent").join("python");
    let mut parent_environment = HashMap::new();
    parent_environment.insert(
        "PYTHONPATH".to_string(),
        parent_python_path.to_string_lossy().into_owned(),
    );
    parent_environment.insert("VX_INTERNAL_MARKER".to_string(), "do-not-leak".to_string());

    let manager = EnvironmentManager::with_parent_environment(
        ContextConfig {
            inherit_parent_env: false,
            parent_variables: vec!["PYTHONPATH".to_string()],
            path_strategy: PathStrategy::NoModify,
            ..Default::default()
        },
        parent_environment,
    );

    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let environment = runtime
        .block_on(manager.generate_environment(&[package]))
        .expect("environment generation succeeds");

    let expected = std::env::join_paths([
        PathBuf::from(format!("{}/python", package_root.to_string_lossy())),
        parent_python_path,
    ])
    .expect("test paths can be joined")
    .to_string_lossy()
    .into_owned();
    assert_eq!(environment.get("PYTHONPATH"), Some(&expected));
    assert!(!environment.contains_key("VX_INTERNAL_MARKER"));
}

#[test]
fn all_parent_variables_inherits_only_variables_touched_by_rex_actions() {
    let mut package = package_with_python_path("bundle/sdk/2.0.0");
    package.commands = Some("env.append_path('SDK_PATH', '{root}/lib')".to_string());
    let parent_sdk_path = PathBuf::from("parent").join("sdk");
    let mut parent_environment = HashMap::new();
    parent_environment.insert(
        "SDK_PATH".to_string(),
        parent_sdk_path.to_string_lossy().into_owned(),
    );
    parent_environment.insert("UNTOUCHED_PARENT".to_string(), "private".to_string());

    let manager = EnvironmentManager::with_parent_environment(
        ContextConfig {
            inherit_parent_env: false,
            all_parent_variables: true,
            path_strategy: PathStrategy::NoModify,
            ..Default::default()
        },
        parent_environment,
    );

    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let environment = runtime
        .block_on(manager.generate_environment(&[package]))
        .expect("environment generation succeeds");

    let expected = std::env::join_paths([parent_sdk_path, PathBuf::from("bundle/sdk/2.0.0/lib")])
        .expect("test paths can be joined")
        .to_string_lossy()
        .into_owned();
    assert_eq!(environment.get("SDK_PATH"), Some(&expected));
    assert!(!environment.contains_key("UNTOUCHED_PARENT"));
}

#[test]
fn set_if_empty_treats_a_parent_variable_as_already_defined() {
    let mut package = package_with_python_path("bundle/tool/1.0.0");
    package.commands = Some("env.setenv_if_empty('LICENSE_SERVER', 'fallback')".to_string());
    let mut parent_environment = HashMap::new();
    parent_environment.insert("LICENSE_SERVER".to_string(), "parent-license".to_string());

    let manager = EnvironmentManager::with_parent_environment(
        ContextConfig {
            inherit_parent_env: false,
            path_strategy: PathStrategy::NoModify,
            ..Default::default()
        },
        parent_environment,
    );
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let environment = runtime
        .block_on(manager.generate_environment(&[package]))
        .expect("environment generation succeeds");

    assert!(!environment.contains_key("LICENSE_SERVER"));
}

#[test]
fn resetenv_restores_the_original_parent_value() {
    let mut package = package_with_python_path("bundle/tool/1.0.0");
    package.commands = Some(
        "env.setenv('LICENSE_SERVER', 'package-license')\nresetenv('LICENSE_SERVER')".to_string(),
    );
    let mut parent_environment = HashMap::new();
    parent_environment.insert("LICENSE_SERVER".to_string(), "parent-license".to_string());

    let manager = EnvironmentManager::with_parent_environment(
        ContextConfig {
            inherit_parent_env: false,
            path_strategy: PathStrategy::NoModify,
            ..Default::default()
        },
        parent_environment,
    );
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let environment = runtime
        .block_on(manager.generate_environment(&[package]))
        .expect("environment generation succeeds");

    assert_eq!(
        environment.get("LICENSE_SERVER").map(String::as_str),
        Some("parent-license")
    );
}

#[test]
fn rex_values_expand_variables_from_the_parent_environment() {
    let mut package = package_with_python_path("bundle/tool/1.0.0");
    package.commands = Some("env.setenv('TOOL_CONFIG', '${HOME}/.tool')".to_string());
    let mut parent_environment = HashMap::new();
    parent_environment.insert("HOME".to_string(), "/parent/home".to_string());

    let manager = EnvironmentManager::with_parent_environment(
        ContextConfig {
            inherit_parent_env: false,
            path_strategy: PathStrategy::NoModify,
            ..Default::default()
        },
        parent_environment,
    );
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let environment = runtime
        .block_on(manager.generate_environment(&[package]))
        .expect("environment generation succeeds");

    assert_eq!(
        environment.get("TOOL_CONFIG").map(String::as_str),
        Some("/parent/home/.tool")
    );
}

#[test]
fn package_metadata_includes_official_rez_prefixed_variables() {
    let package_root = PathBuf::from("bundle").join("python.tools").join("3-11-4");
    let mut package = package_with_python_path(&package_root.to_string_lossy());
    package.name = "python.tools".to_string();
    package.version = Some(Version::parse("3-11-4").expect("valid test version"));
    let manager = EnvironmentManager::with_parent_environment(
        ContextConfig {
            inherit_parent_env: false,
            path_strategy: PathStrategy::NoModify,
            ..Default::default()
        },
        HashMap::new(),
    );
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let environment = runtime
        .block_on(manager.generate_environment(&[package]))
        .expect("environment generation succeeds");
    let expected_root = package_root.to_string_lossy().into_owned();

    assert_eq!(
        environment.get("REZ_PYTHON_TOOLS_ROOT"),
        Some(&expected_root)
    );
    assert_eq!(
        environment.get("REZ_PYTHON_TOOLS_BASE"),
        Some(&expected_root)
    );
    assert_eq!(
        environment
            .get("REZ_PYTHON_TOOLS_VERSION")
            .map(String::as_str),
        Some("3-11-4")
    );
    assert_eq!(
        environment
            .get("REZ_PYTHON_TOOLS_MAJOR_VERSION")
            .map(String::as_str),
        Some("3")
    );
    assert_eq!(
        environment
            .get("REZ_PYTHON_TOOLS_MINOR_VERSION")
            .map(String::as_str),
        Some("11")
    );
    assert_eq!(
        environment
            .get("REZ_PYTHON_TOOLS_PATCH_VERSION")
            .map(String::as_str),
        Some("4")
    );
}

#[test]
fn package_metadata_includes_empty_version_components_for_unversioned_packages() {
    let package_root = PathBuf::from("bundle").join("unversioned_tool");
    let mut package = package_with_python_path(&package_root.to_string_lossy());
    package.name = "unversioned_tool".to_string();
    package.version = None;
    let manager = EnvironmentManager::with_parent_environment(
        ContextConfig {
            inherit_parent_env: false,
            path_strategy: PathStrategy::NoModify,
            ..Default::default()
        },
        HashMap::new(),
    );
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let environment = runtime
        .block_on(manager.generate_environment(&[package]))
        .expect("environment generation succeeds");

    for variable in [
        "REZ_UNVERSIONED_TOOL_VERSION",
        "REZ_UNVERSIONED_TOOL_MAJOR_VERSION",
        "REZ_UNVERSIONED_TOOL_MINOR_VERSION",
        "REZ_UNVERSIONED_TOOL_PATCH_VERSION",
    ] {
        assert_eq!(
            environment.get(variable).map(String::as_str),
            Some(""),
            "{variable} should exist with an empty value"
        );
    }
}

#[test]
fn resolve_membership_conditions_use_the_final_context_packages() {
    let package = PythonAstParser::parse_package_py(
        r#"
name = "python"
version = "3.9.13"

def commands():
    if "python_embedded" not in resolve:
        env.PYTHON_MODE = "standalone"
    else:
        env.PYTHON_MODE = "embedded"
    if building:
        env.PYTHON_BUILD_MODE = "1"
"#,
    )
    .expect("resolve membership should be deferred until context generation");
    let embedded = Package::new("python_embedded".to_string());
    let manager = EnvironmentManager::with_parent_environment(
        ContextConfig {
            inherit_parent_env: false,
            path_strategy: PathStrategy::NoModify,
            ..Default::default()
        },
        HashMap::new(),
    );
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");

    let standalone_environment = runtime
        .block_on(manager.generate_environment(std::slice::from_ref(&package)))
        .expect("standalone environment generation succeeds");
    assert_eq!(
        standalone_environment
            .get("PYTHON_MODE")
            .map(String::as_str),
        Some("standalone")
    );

    let embedded_environment = runtime
        .block_on(manager.generate_environment(&[package, embedded]))
        .expect("embedded environment generation succeeds");
    assert_eq!(
        embedded_environment.get("PYTHON_MODE").map(String::as_str),
        Some("embedded")
    );
    assert!(!embedded_environment.contains_key("PYTHON_BUILD_MODE"));

    let build_manager = EnvironmentManager::with_parent_environment(
        ContextConfig {
            inherit_parent_env: false,
            additional_env_vars: HashMap::from([("REZ_BUILD_ENV".to_string(), "1".to_string())]),
            path_strategy: PathStrategy::NoModify,
            ..Default::default()
        },
        HashMap::new(),
    );
    let build_environment = runtime
        .block_on(
            build_manager.generate_environment(&[PythonAstParser::parse_package_py(
                r#"
name = "python"
version = "3.9.13"
def commands():
    if building:
        env.PYTHON_BUILD_MODE = "1"
"#,
            )
            .expect("building should be deferred until context generation")]),
        )
        .expect("build environment generation succeeds");
    assert_eq!(
        build_environment
            .get("PYTHON_BUILD_MODE")
            .map(String::as_str),
        Some("1")
    );
}

#[test]
fn dynamic_build_only_commands_do_not_block_runtime_contexts() {
    let package = PythonAstParser::parse_package_py(
        r#"
name = "compiler"
version = "1.0.0"

def commands():
    if building:
        import subprocess
        process = subprocess.getoutput("discover-toolchain")
        for line in process.splitlines():
            setenv("DISCOVERED_VALUE", line)
"#,
    )
    .expect("build-only commands should be deferred at package load time");
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let runtime_manager = EnvironmentManager::with_parent_environment(
        ContextConfig {
            inherit_parent_env: false,
            path_strategy: PathStrategy::NoModify,
            ..Default::default()
        },
        HashMap::new(),
    );
    let environment = runtime
        .block_on(runtime_manager.generate_environment(std::slice::from_ref(&package)))
        .expect("runtime environment generation succeeds");
    assert!(!environment.contains_key("DISCOVERED_VALUE"));

    let build_manager = EnvironmentManager::with_parent_environment(
        ContextConfig {
            inherit_parent_env: false,
            additional_env_vars: HashMap::from([("REZ_BUILD_ENV".to_string(), "1".to_string())]),
            path_strategy: PathStrategy::NoModify,
            ..Default::default()
        },
        HashMap::new(),
    );
    let error = runtime
        .block_on(build_manager.generate_environment(&[package]))
        .expect_err("unsupported dynamic build commands must fail when activated");
    assert!(error.to_string().contains("Dynamic package command block"));
}

#[test]
fn dynamic_ephemeral_commands_use_the_resolved_context() {
    let package = PythonAstParser::parse_package_py(
        r#"
name = "compiler"
version = "1.0.0"

def commands():
    feature_build = "feature_build" in ephemerals
    if building or feature_build:
        import subprocess
        subprocess.getoutput("discover-toolchain")
"#,
    )
    .expect("ephemeral command guards should be deferred");
    let mut context = ResolvedContext::from_requirements(Vec::new());
    context.config.path_strategy = PathStrategy::NoModify;
    context.resolved_packages = vec![package];
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");

    runtime
        .block_on(context.get_environ(Some(HashMap::new())))
        .expect("ordinary runtime contexts should skip the dynamic block");

    context.resolved_ephemerals = vec![PackageRequirement::parse(".feature_build").unwrap()];
    let error = runtime
        .block_on(context.get_environ(Some(HashMap::new())))
        .expect_err("the resolved ephemeral should activate the guarded block");
    assert!(error.to_string().contains("'.feature_build' contexts"));
}
