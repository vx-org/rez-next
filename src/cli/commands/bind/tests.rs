//! Unit tests for the bind command sub-modules.
//!
//! Grouped into:
//! - `test_parse`      — package spec parsing
//! - `test_utils`      — version extraction / parsing / close-match helpers
//! - `test_package_gen` — package.py generation + default metadata
//! - `test_detect`     — platform package detection

use super::package_gen::{
    generate_package_py, get_default_requirements, get_default_tools, get_package_commands,
    get_package_description,
};
use super::utils::{
    extract_version_from_output, find_close_matches, get_bind_modules, parse_version_from_string,
};
use super::{detect, parse_package_spec};

#[test]
fn missing_tool_does_not_create_a_fabricated_version() {
    let temp_dir = tempfile::tempdir().unwrap();
    let args = super::BindArgs {
        package: Some("__nonexistent_tool_xyz__".to_string()),
        version: None,
        quickstart: false,
        release: false,
        install_path: Some(temp_dir.path().to_path_buf()),
        no_deps: true,
        list: false,
        search: false,
        verbose: false,
        bind_args: Vec::new(),
    };

    let result =
        super::bind_single_package("__nonexistent_tool_xyz__", temp_dir.path(), true, &args);

    assert!(matches!(
        result,
        Err(rez_next_common::RezCoreError::ExecutionError(_))
    ));
    assert!(!temp_dir.path().join("__nonexistent_tool_xyz__").exists());
}

#[test]
fn explicit_version_allows_binding_without_host_detection() {
    let temp_dir = tempfile::tempdir().unwrap();
    let args = super::BindArgs {
        package: Some("__nonexistent_tool_xyz__".to_string()),
        version: Some("9.8.7".to_string()),
        quickstart: false,
        release: false,
        install_path: Some(temp_dir.path().to_path_buf()),
        no_deps: true,
        list: false,
        search: false,
        verbose: false,
        bind_args: Vec::new(),
    };

    let result =
        super::bind_single_package("__nonexistent_tool_xyz__", temp_dir.path(), true, &args)
            .unwrap();

    assert_eq!(result.package.version.unwrap().to_string(), "9.8.7");
    assert!(result.install_path.join("package.py").exists());
}

// ─── test_parse ──────────────────────────────────────────────────────────────

mod test_parse {
    use super::*;

    #[test]
    fn plain_name_has_no_version() {
        let (name, ver) = parse_package_spec("python").unwrap();
        assert_eq!(name, "python");
        assert_eq!(ver, None);
    }

    #[test]
    fn name_with_version() {
        let (name, ver) = parse_package_spec("python-3.9").unwrap();
        assert_eq!(name, "python");
        assert_eq!(ver, Some("3.9".to_string()));
    }

    #[test]
    fn hyphenated_name_without_version() {
        // "my-package-name" — last segment starts with a letter, not a digit
        let (name, ver) = parse_package_spec("my-package-name").unwrap();
        assert_eq!(name, "my-package-name");
        assert_eq!(ver, None);
    }

    #[test]
    fn hyphenated_name_with_version() {
        let (name, ver) = parse_package_spec("my-pkg-1.2.3").unwrap();
        assert_eq!(name, "my-pkg");
        assert_eq!(ver, Some("1.2.3".to_string()));
    }

    #[test]
    fn single_digit_version() {
        let (name, ver) = parse_package_spec("cmake-3").unwrap();
        assert_eq!(name, "cmake");
        assert_eq!(ver, Some("3".to_string()));
    }
}

// ─── test_utils ──────────────────────────────────────────────────────────────

mod test_utils {
    use super::*;

    // parse_version_from_string

    #[test]
    fn version_from_python_output() {
        assert_eq!(
            parse_version_from_string("3.9.7 (default, Sep 16 2021, 13:09:03)"),
            Some("3.9.7".to_string())
        );
    }

    #[test]
    fn version_from_git_output() {
        assert_eq!(
            parse_version_from_string("git version 2.34.1"),
            Some("2.34.1".to_string())
        );
    }

    #[test]
    fn version_no_digits_returns_none() {
        assert_eq!(parse_version_from_string("no version here"), None);
    }

    #[test]
    fn version_empty_string_returns_none() {
        assert_eq!(parse_version_from_string(""), None);
    }

    #[test]
    fn version_leading_text() {
        // "gcc (Ubuntu 11.4.0) 11.4.0" — first digit sequence is "11"
        let result = parse_version_from_string("gcc (Ubuntu 11.4.0) 11.4.0");
        assert!(result.is_some());
        // Must start with "11"
        assert!(result.unwrap().starts_with("11"));
    }

    #[test]
    fn version_trailing_dots_trimmed() {
        // A trailing dot after the version should be stripped
        let result = parse_version_from_string("3.9.0.");
        assert_eq!(result, Some("3.9.0".to_string()));
    }

    // extract_version_from_output

    #[test]
    fn extract_python_version() {
        assert_eq!(
            extract_version_from_output("Python 3.10.12\n", "Python"),
            Some("3.10.12".to_string())
        );
    }

    #[test]
    fn extract_pip_version() {
        let output = "pip 23.1 from /usr/lib/python3/dist-packages/pip (python 3.10)\n";
        assert_eq!(
            extract_version_from_output(output, "pip"),
            Some("23.1".to_string())
        );
    }

    #[test]
    fn extract_cmake_version() {
        let output = "cmake version 3.26.4\n\nCMake suite maintained and supported by Kitware (kitware.com/cmake).\n";
        assert_eq!(
            extract_version_from_output(output, "cmake version"),
            Some("3.26.4".to_string())
        );
    }

    #[test]
    fn extract_missing_prefix_returns_none() {
        assert_eq!(
            extract_version_from_output("no relevant output", "Python"),
            None
        );
    }

    #[test]
    fn extract_case_insensitive() {
        // Prefix match should be case-insensitive
        assert_eq!(
            extract_version_from_output("PYTHON 3.11.0", "python"),
            Some("3.11.0".to_string())
        );
    }

    // find_close_matches

    #[test]
    fn close_match_exact_substring() {
        let modules = get_bind_modules().unwrap();
        let matches = find_close_matches("python", &modules);
        let names: Vec<&str> = matches.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"python"),
            "should find 'python' by exact match"
        );
    }

    #[test]
    fn close_match_no_result_for_unknown() {
        let modules = get_bind_modules().unwrap();
        let matches = find_close_matches("zzznomatch", &modules);
        assert!(matches.is_empty());
    }

    #[test]
    fn close_match_results_sorted() {
        let modules = get_bind_modules().unwrap();
        // Both "gcc" and "clang" should *not* match "python"
        let matches = find_close_matches("git", &modules);
        let names: Vec<String> = matches.iter().map(|(n, _)| n.clone()).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "results should be lexicographically sorted");
    }

    #[test]
    fn cli_modules_include_every_builtin_binder() {
        let modules = get_bind_modules().unwrap();
        for binder in rez_next_bind::builtin_binders() {
            assert!(
                modules.contains_key(binder.name),
                "CLI module list is missing crate binder '{}'",
                binder.name
            );
        }
    }

    #[test]
    fn builtin_modules_use_descriptor_marker() {
        let modules = get_bind_modules().unwrap();
        let python = modules.get("python").unwrap();
        assert_eq!(python.path.to_string_lossy(), "<built-in>");
    }
}

// ─── test_package_gen ────────────────────────────────────────────────────────

mod test_package_gen {
    use super::*;

    #[test]
    fn generate_basic_package_py() {
        let content = generate_package_py(
            "python",
            "3.9.7",
            "Python interpreter",
            &["os".to_string()],
            &["python".to_string()],
            None,
        );
        assert!(content.contains("name = 'python'"));
        assert!(content.contains("version = '3.9.7'"));
        assert!(content.contains("requires = ["));
        assert!(content.contains("'os'"));
        assert!(content.contains("tools = ["));
        assert!(
            !content.contains("def commands()"),
            "no commands block expected"
        );
    }

    #[test]
    fn generate_package_py_with_commands() {
        let content = generate_package_py(
            "cmake",
            "3.26.4",
            "CMake",
            &[],
            &["cmake".to_string()],
            Some("    env.PATH.prepend('{root}/bin')\n"),
        );
        assert!(content.contains("def commands():"));
        assert!(content.contains("env.PATH.prepend"));
    }

    #[test]
    fn generate_package_py_no_requires_no_tools() {
        let content = generate_package_py("platform", "1.0.0", "Platform", &[], &[], None);
        assert!(!content.contains("requires = ["));
        assert!(!content.contains("tools = ["));
    }

    #[test]
    fn default_requirements_hierarchy() {
        assert_eq!(get_default_requirements("platform"), Vec::<String>::new());
        assert_eq!(
            get_default_requirements("os"),
            vec!["platform".to_string(), "arch".to_string()]
        );
        assert_eq!(get_default_requirements("python"), vec!["os".to_string()]);
        assert_eq!(get_default_requirements("pip"), vec!["python".to_string()]);
    }

    #[test]
    fn default_tools_mapping() {
        assert_eq!(get_default_tools("platform"), Vec::<String>::new());
        let python_tools = get_default_tools("python");
        assert!(python_tools.contains(&"python".to_string()));
        assert!(python_tools.contains(&"python3".to_string()));
        let cmake_tools = get_default_tools("cmake");
        assert!(cmake_tools.contains(&"cmake".to_string()));
        assert!(cmake_tools.contains(&"ctest".to_string()));
        assert!(cmake_tools.contains(&"cpack".to_string()));
    }

    #[test]
    fn package_description_known_names() {
        assert!(!get_package_description("python").is_empty());
        assert!(!get_package_description("git").is_empty());
        assert!(!get_package_description("rez").is_empty());
    }

    #[test]
    fn package_description_unknown_name() {
        let desc = get_package_description("unknown_pkg_xyz");
        assert!(
            desc.contains("unknown_pkg_xyz"),
            "should include the package name"
        );
    }

    #[test]
    fn package_commands_python_no_path() {
        let cmd = get_package_commands("python", None);
        // Falls back to {root}/bin template
        assert!(cmd.is_some());
        assert!(cmd.unwrap().contains("{root}/bin"));
    }

    #[test]
    fn package_commands_unknown_name_returns_none() {
        let cmd = get_package_commands("unknown_tool", None);
        assert!(cmd.is_none());
    }
}

// ─── test_detect ─────────────────────────────────────────────────────────────

mod test_detect {
    use super::detect;

    #[test]
    fn detect_platform_returns_some() {
        assert!(detect::detect_system_tool("platform").is_some());
    }

    #[test]
    fn detect_arch_returns_some() {
        assert!(detect::detect_system_tool("arch").is_some());
    }

    #[test]
    fn detect_rez_returns_some() {
        let tool = detect::detect_system_tool("rez");
        assert!(tool.is_some());
        let tool = tool.unwrap();
        assert!(!tool.version.is_empty());
    }

    #[test]
    fn detect_os_returns_some() {
        assert!(detect::detect_system_tool("os").is_some());
    }

    #[test]
    fn detect_unknown_returns_none() {
        assert!(detect::detect_system_tool("nonexistent_tool_xyz").is_none());
    }

    #[test]
    fn executable_detector_names_match_builtin_binders() {
        for binder in rez_next_bind::builtin_binders() {
            let detected = detect::detect_system_tool(binder.name);
            assert_eq!(
                detected.is_some(),
                rez_next_bind::detect_builtin_tool(binder.name).is_ok(),
                "CLI and crate detectors disagree for '{}'",
                binder.name
            );
        }
    }

    #[test]
    fn platform_metadata_key_present() {
        let tool = detect::detect_system_tool("platform").unwrap();
        assert!(
            tool.metadata.contains_key("platform"),
            "platform metadata should have 'platform' key"
        );
    }

    #[test]
    fn arch_metadata_key_present() {
        let tool = detect::detect_system_tool("arch").unwrap();
        assert!(
            tool.metadata.contains_key("arch"),
            "arch metadata should have 'arch' key"
        );
    }
}
