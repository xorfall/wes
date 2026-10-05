use wes_adapters::api_library::{Library, PackageKey, Repository, digest};
fn key() -> PackageKey {
    PackageKey {
        service: "catalogue".into(),
        api_version: "v1".into(),
        scope: "public".into(),
    }
}
fn descriptor() -> &'static [u8] {
    include_bytes!("../../../examples/api-import/inventory.json")
}

#[test]
fn immutable_objects_sources_and_index_survive_reopen_and_ignore_unpublished_orphans() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap().join("library");
    let mut store = Library::open(&root).unwrap();
    let source = b"complete source\nwith prose limits\n";
    let package = store
        .save(
            key(),
            descriptor(),
            "synthetic-ingest".into(),
            Some(source),
            false,
        )
        .unwrap();
    assert_eq!(
        std::fs::read(root.join(format!("sources/{}.txt", digest(source)))).unwrap(),
        source
    );
    let path = store.descriptor_path(&package.revision).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), descriptor());
    assert_eq!(
        store
            .save(
                key(),
                descriptor(),
                "another source".into(),
                Some(source),
                false
            )
            .unwrap(),
        package
    );
    assert_eq!(store.index.packages.len(), 1);
    drop(store);
    std::fs::write(root.join(".api-write-crash-fixture"), "unpublished").unwrap();
    let reopened = Library::open(&root).unwrap();
    assert_eq!(reopened.index.packages.len(), 1);
    assert_eq!(
        reopened.descriptor(&package.revision).unwrap(),
        descriptor()
    );
}
#[test]
fn unrelated_directories_unsupported_descriptors_and_hash_tampering_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let root = base.join("unrelated");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("mine.txt"), "original").unwrap();
    assert!(Library::open(&root).is_err());
    assert_eq!(std::fs::read(root.join("mine.txt")).unwrap(), b"original");
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    let mut library = Library::open(&base.join("library")).unwrap();
    assert!(
        library
            .save(key(), b"{}", "invalid".into(), None, false)
            .is_err()
    );
    assert!(
        library
            .save(
                PackageKey {
                    service: "../escape".into(),
                    ..key()
                },
                descriptor(),
                "invalid".into(),
                None,
                false
            )
            .is_err()
    );
    let p = library
        .save(key(), descriptor(), "valid".into(), None, false)
        .unwrap();
    let path = library.descriptor_path(&p.revision).unwrap();
    std::fs::write(path, b"{}").unwrap();
    assert!(library.descriptor(&p.revision).is_err());
}
#[cfg(unix)]
#[test]
fn symlinked_repository_artifacts_and_library_paths_cannot_escape() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let outside = base.join("outside");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, base.join("library-link")).unwrap();
    assert!(Library::open(&base.join("library-link")).is_err());
    let root = base.join("library");
    let mut library = Library::open(&root).unwrap();
    let p = library
        .save(key(), descriptor(), "synthetic".into(), None, false)
        .unwrap();
    let path = library.descriptor_path(&p.revision).unwrap();
    std::fs::rename(&path, outside.join("descriptor.json")).unwrap();
    symlink(outside.join("descriptor.json"), &path).unwrap();
    assert!(library.descriptor(&p.revision).is_err());
}
#[test]
fn github_repository_configuration_is_explicitly_commit_pinned_and_confined() {
    let good = Repository::Github {
        owner: "example".into(),
        repository: "catalog".into(),
        commit: "a".repeat(40),
        prefix: "apis".into(),
    };
    assert!(good.validate().is_ok());
    for (commit, prefix) in [
        ("main", ""),
        ("a", ""),
        (&"a".repeat(40), "../outside"),
        (&"a".repeat(40), "/absolute"),
    ] {
        assert!(
            Repository::Github {
                owner: "example".into(),
                repository: "catalog".into(),
                commit: commit.into(),
                prefix: prefix.into()
            }
            .validate()
            .is_err()
        );
    }
}

#[test]
fn portable_source_identifiers_remove_parent_paths_and_url_credentials() {
    use wes_adapters::api_library::portable_source_location;
    assert_eq!(
        portable_source_location("/private/author/specs/api.yaml"),
        "api.yaml"
    );
    assert_eq!(
        portable_source_location(r"C:\private\author\specs\api.yaml"),
        "api.yaml"
    );
    assert_eq!(
        portable_source_location(
            "https://user:private@example.invalid/api.yaml?token=private#fragment"
        ),
        "https://example.invalid/api.yaml"
    );
}
