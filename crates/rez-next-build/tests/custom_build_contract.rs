use std::path::PathBuf;

use rez_next_build::{BuildConfig, BuildManager, BuildRequest};
use rez_next_package::Package;
use rez_next_version::Version;

#[tokio::test]
async fn custom_build_system_runs_build_command_and_installs_definition() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    let build_root = temporary.path().join("build");
    let install_root = temporary.path().join("repository");
    tokio::fs::create_dir_all(&source).await.unwrap();
    tokio::fs::write(
        source.join("package.py"),
        "name = 'custom_contract'\nversion = '1.0.0'\n",
    )
    .await
    .unwrap();

    let mut package = Package::new("custom_contract".to_string());
    package.version = Some(Version::parse("1.0.0").unwrap());
    package.build_system = Some("custom".to_string());
    package.build_command = Some("echo ok > marker.txt".to_string());

    let config = BuildConfig {
        build_dir: build_root.clone(),
        ..BuildConfig::default()
    };
    let mut manager = BuildManager::with_config(config);
    let mut request = BuildRequest::new(package, None, PathBuf::from(&source));
    request.install_path = Some(install_root.clone());

    let build_ids = manager.start_build(request).await.unwrap();
    let result = manager.wait_for_build(&build_ids[0]).await.unwrap();

    assert!(result.success, "custom build failed: {}", result.errors);
    assert_eq!(
        tokio::fs::read_to_string(build_root.join("custom_contract").join("marker.txt"))
            .await
            .unwrap()
            .trim(),
        "ok"
    );
    assert!(
        install_root
            .join("custom_contract")
            .join("1.0.0")
            .join("package.py")
            .is_file()
    );
}
