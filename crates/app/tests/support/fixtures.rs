//! Portable binaries built from the checked-in example source, kept inside the test's project.
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

#[allow(dead_code)]
pub fn echo(project: &Path) -> PathBuf {
    static BUILT: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    let (_, executable) = BUILT.get_or_init(|| {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("example-echo.bin");
        let rustc = Path::new(env!("CARGO")).with_file_name(if cfg!(windows) {
            "rustc.exe"
        } else {
            "rustc"
        });
        let output = std::process::Command::new(rustc)
            .arg("--edition=2024")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/support/echo.rs"))
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "portable fixture build: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        (directory, executable)
    });
    let path = project.join("example-echo.bin");
    if !path.exists() {
        std::fs::copy(executable, &path).unwrap();
    }
    path
}
