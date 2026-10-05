//! Generate the test fixture from the authoritative core schema.
fn main() {
    println!(
        "{}",
        serde_json::to_string_pretty(&wes::web::language::published_yaml()).unwrap()
    );
}
