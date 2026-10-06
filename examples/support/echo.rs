//! A portable argument-printing fixture for examples; it is not part of the application.
fn main() {
    println!("{}", std::env::args().skip(1).collect::<Vec<_>>().join(" "));
}
