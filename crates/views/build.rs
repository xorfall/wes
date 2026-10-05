#[allow(dead_code)]
#[path = "src/package.rs"]
mod package;
pub use package::Package;
#[allow(dead_code)]
#[path = "src/artifact.rs"]
mod artifact;
use std::{collections::BTreeSet, env, fs, path::PathBuf};
fn main() {
    let repository = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let root = repository.join("views");
    for dependency in [
        "packages/view-sdk",
        "tools/view-package",
        "package-lock.json",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            repository.join(dependency).display()
        );
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed={}", root.display());
    let mut entries = fs::read_dir(&root)
        .expect("view package directory")
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect::<Vec<_>>();
    entries.sort();
    assert!(entries.len() <= 64, "At most 64 built-in view packages");
    let (mut names, mut ids) = (BTreeSet::new(), BTreeSet::new());
    let mut generated = String::from("const SOURCES: &[&str] = &[\n");
    let mut packages = vec![];
    for directory in entries {
        let manifest = fs::read_to_string(directory.join("view.json")).expect("view.json");
        let types = fs::read_to_string(directory.join("types.yaml")).expect("types.yaml");
        let p = package::Package::parse(&manifest, &types).expect("valid view package");
        assert!(
            names.insert(p.manifest.name.clone()) && ids.insert(p.manifest.id.clone()),
            "Duplicate view name/id"
        );
        assert_eq!(
            directory.file_name().unwrap().to_str().unwrap(),
            p.manifest.id,
            "Package directory must equal its id"
        );
        let renderer = directory
            .join(&p.manifest.renderer)
            .canonicalize()
            .expect("renderer exists");
        assert!(
            renderer.starts_with(directory.canonicalize().unwrap()),
            "Renderer escapes package"
        );
        let working = out.join(&p.manifest.id);
        fs::create_dir_all(&working).unwrap();
        let metadata = working.join("metadata.json");
        fs::write(&metadata,serde_json::to_vec(&serde_json::json!({"manifest":manifest,"types":types,"definition":p.description()})).unwrap()).unwrap();
        let compiled = working.join("view.wes-view.json");
        let status = std::process::Command::new("node")
            .arg(repository.join("tools/view-package/native-build.mjs"))
            .arg(&directory)
            .arg(metadata)
            .arg(&compiled)
            .status()
            .expect("Node.js is required to build shipped Views; install Node.js and run npm ci from the repository root before cargo build");
        assert!(
            status.success(),
            "View compilation failed: {}",
            p.manifest.name
        );
        let bytes = fs::read(&compiled).unwrap();
        let checked = artifact::Artifact::parse(&bytes).expect("valid compiled View");
        generated.push_str(&format!("include_str!({:?}),\n", compiled));
        packages.push(checked.package);
    }
    package::validate_catalogue(packages.iter()).expect("compatible view catalogue");
    generated.push_str("];\n");
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("packages.rs"),
        generated,
    )
    .unwrap();
}
