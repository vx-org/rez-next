//! Tests for package URI functions.
//!
//! These are light smoke tests — the real heavy lifting of URI resolution,
//! variant lookup, and repository interaction is tested end-to-end in the
//! integration test suite (`tests/` directory).

use rez_next_version::Version;

use super::{parse_variant_uri, split_combined_package_uri};

#[test]
fn test_parse_version_for_uri_resolution() {
    // Package URI resolution (get_package_from_uri, etc.) relies on
    // version parsing — verify the dependency chain is wired correctly.
    let v = Version::parse("3.9.5").expect("valid version");
    assert_eq!(v.to_string(), "3.9.5");
}

#[test]
fn test_version_compare_for_variant_matching() {
    // get_variant / get_variant_from_uri need version comparison.
    let v1 = Version::parse("2.0").unwrap();
    let v2 = Version::parse("2.0.0").unwrap();
    // Trailing zeros are implicit (rez semantics).
    assert_eq!(v1, v2);
}

#[test]
fn test_parse_variant_uri_supports_index_and_null_variant() {
    assert_eq!(
        parse_variant_uri("/packages/tool/1.0.0/package.py[2]"),
        Some(("/packages/tool/1.0.0/package.py", Some(2)))
    );
    assert_eq!(
        parse_variant_uri("/packages/tool/1.0.0/package.py[]"),
        Some(("/packages/tool/1.0.0/package.py", None))
    );
    assert_eq!(parse_variant_uri("/packages/tool/1.0.0/package.py"), None);
    assert_eq!(
        parse_variant_uri("/packages/tool/1.0.0/package.py[bad]"),
        None
    );
}

#[test]
fn test_split_combined_package_uri() {
    assert_eq!(
        split_combined_package_uri("/packages/tool/package.py<1.0.0>"),
        ("/packages/tool/package.py", Some("1.0.0"))
    );
    assert_eq!(
        split_combined_package_uri("/packages/tool/1.0.0/package.py"),
        ("/packages/tool/1.0.0/package.py", None)
    );
}

#[test]
fn test_load_package_uri_sets_repository_root_and_filepath() {
    let temp_dir = tempfile::tempdir().unwrap();
    let package_dir = temp_dir.path().join("tool").join("1.0.0");
    std::fs::create_dir_all(&package_dir).unwrap();
    let package_file = package_dir.join("package.py");
    std::fs::write(
        &package_file,
        "name = 'tool'\nversion = '1.0.0'\nvariants = [['python-3.11']]\n",
    )
    .unwrap();

    let (package, loaded_file, repository_root) =
        super::load_package_uri(&package_file.to_string_lossy(), None)
            .unwrap()
            .expect("package should resolve");

    assert_eq!(package.name, "tool");
    assert_eq!(
        package.filepath,
        Some(loaded_file.to_string_lossy().to_string())
    );
    assert_eq!(repository_root, temp_dir.path());
}

#[test]
fn test_load_package_uri_honors_explicit_search_paths() {
    let temp_dir = tempfile::tempdir().unwrap();
    let package_dir = temp_dir.path().join("tool").join("1.0.0");
    std::fs::create_dir_all(&package_dir).unwrap();
    let package_file = package_dir.join("package.py");
    std::fs::write(&package_file, "name = 'tool'\nversion = '1.0.0'\n").unwrap();

    let wrong_path = tempfile::tempdir().unwrap();
    assert!(
        super::load_package_uri(
            &package_file.to_string_lossy(),
            Some(&[wrong_path.path().to_string_lossy().to_string()]),
        )
        .unwrap()
        .is_none()
    );
}
