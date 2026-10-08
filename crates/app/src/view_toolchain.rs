//! Optional developer tooling. Discovery never executes a binary or writes files.
mod files;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
static RESOURCES: OnceLock<PathBuf> = OnceLock::new();
pub fn use_bundled_resources(path: PathBuf) {
    let _ = RESOURCES.set(path);
}
fn validator_name() -> String {
    format!("wes-view-build{}", std::env::consts::EXE_SUFFIX)
}
fn validator() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let parent = executable.parent()?;
    RESOURCES
        .get()
        .cloned()
        .into_iter()
        .chain([parent.join("../Resources"), parent.to_path_buf()])
        .map(|root| root.join(validator_name()))
        .find(|path| path.is_file())
}
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .take(64)
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}
pub fn status() -> Value {
    let native = validator();
    json!({"scope":"Wes backend host, not the selected execution environment or necessarily the agent terminal", "os":std::env::consts::OS,"arch":std::env::consts::ARCH,
        "sdk":wes_views::sdk_version(),"embeddedCompilerSources":true,
        "validator":{"found":native.is_some(),"path":native,"versionExecuted":false},
        "node":{"found":on_path(&format!("node{}",std::env::consts::EXE_SUFFIX)).is_some(),"versionExecuted":false},
        "export":{"available":native.is_some(),"executable":std::env::current_exe().ok(),"arguments":["--export-view-toolchain","NEW_DIRECTORY"]},
        "install":"After explicit export, use Node.js 20+ and npm ci --ignore-scripts --no-audit --no-fund inside the kit. Invoke node wes-view-package.mjs; PATH configuration and a repository checkout are not required.",
        "execution":"This inventory does not run versions, install dependencies, export files, build a package or grant filesystem/execution permissions. Dependencies and compiler readiness in another terminal are not inferred.",
        "missingValidator":"If validator.found is false, this installation lacks the development validator. Install a build containing it; repository developers can build cargo build -p wes-views --bin wes-view-build --locked."})
}
pub fn export(destination: &Path) -> Result<Value, String> {
    export_from(destination, validator().ok_or("The installation lacks wes-view-build. Read view_toolchain for setup; no files were exported.")?)
}
fn export_from(destination: &Path, native: PathBuf) -> Result<Value, String> {
    if !native.is_file() {
        return Err("View validator is unavailable; no files were exported.".into());
    }
    // Exclusive creation claims only this new directory. Never replace an existing path,
    // and retain an incomplete new export for inspection if a subsequent write fails.
    std::fs::create_dir(destination).map_err(|_| "Choose a new directory with an existing writable parent; toolchain export never overwrites.".to_owned())?;
    let write = || -> Result<(), std::io::Error> {
        for (name, contents) in files::FILES {
            let path = destination.join(name);
            std::fs::create_dir_all(path.parent().expect("file parent"))?;
            std::fs::write(path, contents)?;
        }
        std::fs::create_dir(destination.join("bin"))?;
        std::fs::copy(native, destination.join("bin").join(validator_name()))?;
        std::fs::write(
            destination.join("toolchain.json"),
            serde_json::to_vec_pretty(
                &json!({"sdk":wes_views::sdk_version(),"os":std::env::consts::OS,"arch":std::env::consts::ARCH,"validator":validator_name()}),
            )?,
        )?;
        Ok(())
    };
    write().map_err(|_| "Toolchain export is incomplete in the newly created directory. Inspect it before retrying with a new destination.".to_owned())?;
    Ok(
        json!({"ok":true,"directory":destination,"sdk":wes_views::sdk_version(),"dependenciesInstalled":false,"next":"Read README.md, explicitly run npm ci --ignore-scripts --no-audit --no-fund, then node wes-view-package.mjs describe."}),
    )
}
/// Shared early CLI dispatch for the batch and desktop executable; no data home is opened.
pub fn entry() -> Option<u8> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args
        .first()
        .is_none_or(|arg| arg != "--export-view-toolchain")
    {
        return None;
    }
    let result = match args.as_slice() {
        [_, directory] => export(Path::new(directory)),
        _ => Err("Usage: wes --export-view-toolchain NEW_DIRECTORY".into()),
    };
    match result {
        Ok(result) => {
            println!("{result}");
            Some(0)
        }
        Err(message) => {
            eprintln!("{message}");
            Some(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_is_repository_independent_exclusive_and_does_not_install_or_copy_user_data() {
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("validator");
        std::fs::write(&native, b"synthetic validator").unwrap();
        let target = root.path().join("kit");
        assert_eq!(
            export_from(&target, native.clone()).unwrap()["dependenciesInstalled"],
            false
        );
        let lock: Value =
            serde_json::from_slice(&std::fs::read(target.join("package-lock.json")).unwrap())
                .unwrap();
        assert_eq!(lock["packages"]["compiler"]["version"], "1.0.0");
        assert!(target.join("sdk/index.ts").is_file());
        let sdk: Value =
            serde_json::from_slice(&std::fs::read(target.join("sdk/package.json")).unwrap())
                .unwrap();
        for file in sdk["files"].as_array().unwrap() {
            let file = file.as_str().unwrap();
            assert!(
                target.join("sdk").join(file).is_file(),
                "SDK export omits {file}"
            );
        }
        assert!(!target.join("node_modules").exists());
        assert_eq!(
            std::fs::read(target.join("bin").join(validator_name())).unwrap(),
            b"synthetic validator"
        );
        assert!(export_from(&target, native).is_err());
        assert!(
            files::FILES
                .iter()
                .all(|(name, _)| !name.contains("..") && !name.starts_with('/'))
        );
    }
}
