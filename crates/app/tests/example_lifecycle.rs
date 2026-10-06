//! The program an example check owns is stopped and reaped in every outcome, against the
//! real binary: an orderly stop when asked, and a reported forced end when that is impossible.
#[path = "support/python.rs"]
mod python;
#[test]
fn an_examples_owned_program_stops_when_asked_and_leaves_nothing_behind() {
    let checker =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/check-lifecycle.py");
    let output = python::command()
        .arg(checker)
        .arg("--binary")
        .arg(env!("CARGO_BIN_EXE_wes"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
