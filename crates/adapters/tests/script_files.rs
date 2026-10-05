use std::path::Path;
use wes_adapters::script_files::ScriptFile;

#[test]
fn capture_is_exact_bounded_and_independent_of_later_file_changes() {
    let root = tempfile::tempdir().unwrap();
    let text = "\u{feff}#!unchanged\r\n🦀 $HOME\r\n";
    std::fs::write(root.path().join("script"), text).unwrap();
    let capture = ScriptFile::read(root.path(), Path::new("script")).unwrap();
    std::fs::write(root.path().join("script"), "replacement").unwrap();
    assert_eq!(capture.text, text);
    assert_eq!(capture.directory, root.path().canonicalize().unwrap());
    std::fs::write(
        root.path().join("limit"),
        vec![b' '; wes_engine::source::max_source_bytes()],
    )
    .unwrap();
    assert_eq!(
        ScriptFile::read(root.path(), Path::new("limit"))
            .unwrap()
            .text
            .len(),
        wes_engine::source::max_source_bytes()
    );
}

#[cfg(unix)]
#[test]
fn symlink_uses_invoked_directory_not_target_directory() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("target")).unwrap();
    std::fs::write(root.path().join("target/script"), ":help").unwrap();
    std::os::unix::fs::symlink("target/script", root.path().join("entry")).unwrap();
    let capture = ScriptFile::read(root.path(), Path::new("entry")).unwrap();
    assert_eq!(capture.directory, root.path().canonicalize().unwrap());
    assert!(capture.path.ends_with("entry"));
    assert_eq!(capture.text, ":help");
}

#[cfg(unix)]
#[test]
fn fifo_and_non_utf8_paths_are_refused() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(root.path().join("pipe"))
            .status()
            .unwrap()
            .success()
    );
    assert!(ScriptFile::read(root.path(), Path::new("pipe")).is_err());
    let name = std::ffi::OsString::from_vec(vec![0xff]);
    assert!(ScriptFile::read(root.path(), Path::new(&name)).is_err());
}
