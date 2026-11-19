use std::fs;

use rez_next_repository::{get_plugin_packages, get_plugins};
use tempfile::TempDir;

fn write_package(repository: &std::path::Path, name: &str, version: &str, plugin_for: &str) {
    let directory = repository.join(name).join(version);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("package.py"),
        format!("name = {name:?}\nversion = {version:?}\nplugin_for = {plugin_for}\n",),
    )
    .unwrap();
}

#[test]
fn get_plugins_uses_plugin_for_metadata_across_all_versions() {
    let repository = TempDir::new().unwrap();
    write_package(repository.path(), "maya", "2024", "[]");
    write_package(repository.path(), "maya_plugin", "1.0.0", "[]");
    write_package(repository.path(), "maya_plugin", "2.0.0", "['maya']");
    write_package(repository.path(), "tool_for_maya", "1.0.0", "['other']");

    assert_eq!(
        get_plugins(
            "maya",
            Some(vec![repository.path().to_string_lossy().into_owned()]),
        ),
        vec!["maya_plugin".to_string()]
    );

    let packages = get_plugin_packages(
        "maya",
        Some(vec![repository.path().to_string_lossy().into_owned()]),
    );
    assert_eq!(packages.len(), 1);
    assert_eq!(packages[0].version.as_ref().unwrap().as_str(), "2.0.0");
}

#[test]
fn get_plugins_deduplicates_families_across_repositories() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    write_package(first.path(), "maya_plugin", "1.0.0", "['maya']");
    write_package(second.path(), "maya_plugin", "2.0.0", "['maya']");

    let plugins = get_plugin_packages(
        "maya",
        Some(vec![
            first.path().to_string_lossy().into_owned(),
            second.path().to_string_lossy().into_owned(),
        ]),
    );

    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0].name, "maya_plugin");
    assert_eq!(plugins[0].version.as_ref().unwrap().as_str(), "2.0.0");
}
