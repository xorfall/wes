//! Runnable documentation against the real CLI, with an isolated data home.
#[path = "support/python.rs"]
mod python;
#[test]
fn type_discovery_example_and_held_restore() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = python::command()
        .arg(root.join("examples/type-discovery/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python documentation checker");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
