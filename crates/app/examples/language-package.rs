//! Prints the calculation package exactly as `GET /language/calc` serves it.
//!
//! The client bundles a copy so it can still highlight and complete against an engine older than
//! that route. Regenerate the copy with:
//!
//! ```sh
//! cargo run -p wes --example language-package > gui/src/surface/language-default.json
//! ```
//!
//! `bundled_package_matches_the_served_package` fails when the copy and the engine disagree.
fn main() {
    let package = wes_language::calc::Package::standard();
    println!(
        "{}",
        serde_json::to_string_pretty(&wes::web::language::published(&package))
            .expect("the published package serialises")
    );
}
