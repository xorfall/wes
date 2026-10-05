fn main() {
    println!("cargo::rerun-if-changed=../../packages/budgets/catalog.json");
}
