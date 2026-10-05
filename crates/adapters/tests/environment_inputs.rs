//! Local environment failures remain actionable without widening the core error model.
use wes_adapters::environments::LocalEnvironments;
use wes_engine::environments::EnvironmentLoader;

#[test]
fn package_read_failures_preserve_reason_path_and_remedy_without_contents() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let loader = LocalEnvironments::new(&base).unwrap();
    let missing = loader.capture("missing.yaml").err().unwrap();
    assert_eq!(missing.code, "ENV010");
    assert!(
        missing
            .message
            .contains("environment package: file not found")
    );
    assert!(
        missing
            .message
            .contains(&format!("{:?}", base.join("missing.yaml")))
    );
    assert!(
        missing
            .message
            .contains(&format!("Relative-path base: {base:?}"))
    );
    assert!(missing.message.contains("absolute file path"));

    std::fs::create_dir(base.join("directory")).unwrap();
    std::fs::write(base.join("binary"), b"secret-content\xff").unwrap();
    let large = std::fs::File::create(base.join("large")).unwrap();
    large.set_len(1024 * 1024 + 1).unwrap();
    for (path, reason) in [
        ("directory", "not a regular file"),
        ("binary", "not valid UTF-8 text"),
        (
            "large",
            "file too large (at least 1048577 bytes; limit 1048576 bytes)",
        ),
        ("bad\npath", "invalid local input path"),
    ] {
        let error = loader.capture(path).err().unwrap();
        assert_eq!(error.code, "ENV010");
        assert!(error.message.contains(reason), "{error}");
        assert!(!error.message.contains("secret-content"));
        assert!(!error.message.contains('\n'));
    }
}

#[test]
fn absolute_packages_and_package_relative_descriptors_and_locks_keep_their_paths() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path().canonicalize().unwrap();
    let package_dir = base.join("nested");
    std::fs::create_dir(&package_dir).unwrap();
    let package = package_dir.join("environments.yaml");
    std::fs::write(
        &package,
        include_str!("../../../examples/http-inspection/environments.yaml"),
    )
    .unwrap();
    let loader = LocalEnvironments::new(&base).unwrap();
    let error = loader.capture(package.to_str().unwrap()).err().unwrap();
    assert!(
        error
            .message
            .contains("environment descriptor: file not found")
    );
    assert!(
        error
            .message
            .contains(&format!("{:?}", package_dir.join("sensor.json")))
    );
    assert!(
        error
            .message
            .contains(&format!("Relative-path base: {package_dir:?}"))
    );
    std::fs::write(
        package_dir.join("sensor.json"),
        include_str!("../../../examples/http-inspection/sensor.json"),
    )
    .unwrap();
    assert!(loader.capture(package.to_str().unwrap()).is_ok());
    assert!(loader.capture("nested/environments.yaml").is_ok());

    let missing = base.join("missing.yaml");
    let error = loader.capture(missing.to_str().unwrap()).err().unwrap();
    assert!(!error.message.contains("Relative-path base:"));
    assert!(error.message.contains(&format!("{missing:?}")));
    let error = loader.read_lock("missing.lock.json").err().unwrap();
    assert!(error.message.contains("environment lock: file not found"));
    assert!(
        error
            .message
            .contains(&format!("{:?}", base.join("missing.lock.json")))
    );

    #[cfg(unix)]
    {
        // The descriptor still resolves from the selected package's parent, as before.
        std::os::unix::fs::symlink(&package, package_dir.join("linked.yaml")).unwrap();
        assert!(loader.capture("nested/linked.yaml").is_ok());
    }
}
