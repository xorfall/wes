//! Standalone contract validation/code-generation input. Does not execute view code.
use std::{fs::File, io::Read, path::Path};

fn bounded(path: &Path, limit: usize) -> Result<String, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds its size limit", path.display()));
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

fn run() -> Result<serde_json::Value, String> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [flag] if flag == "--assets" => Ok(serde_json::json!(wes_views::artifacts().iter().map(|a|
            serde_json::json!({"digest":a.digest,"definition":a.package.description(),"javascript":a.source.javascript,"css":a.source.css})
        ).collect::<Vec<_>>())),
        [] => Ok(serde_json::json!(wes_views::catalogue().iter().map(|p|
            serde_json::json!({"directory":p.manifest.id,"renderer":p.manifest.renderer,"definition":p.description()})
        ).collect::<Vec<_>>())),
        [flag, directory] if flag == "--source" => {
            let root = Path::new(directory).canonicalize().map_err(|e| e.to_string())?;
            let manifest = bounded(&root.join("view.json"), 64 * 1024)?;
            let types = bounded(&root.join("types.yaml"), 256 * 1024)?;
            let package = wes_views::Package::parse(&manifest, &types)?;
            let renderer = root.join(&package.manifest.renderer).canonicalize().map_err(|e| e.to_string())?;
            if !renderer.starts_with(&root) { return Err("Renderer escapes its source package".into()); }
            Ok(serde_json::json!({"manifest":manifest,"types":types,"definition":package.description()}))
        }
        [flag, artifact] if flag == "--artifact" => {
            let bytes = bounded(Path::new(artifact), wes_views::MAX_ARTIFACT_BYTES)?;
            let artifact = wes_views::Artifact::parse(bytes.as_bytes())?;
            Ok(serde_json::json!({"digest":artifact.digest,"definition":artifact.package.description()}))
        }
        _ => Err("Usage: wes-view-build [--assets | --source SOURCE_DIR | --artifact COMPILED_FILE]".into()),
    }
}
fn main() {
    match run() {
        Ok(value) => println!(
            "{}",
            serde_json::to_string_pretty(&value).expect("view metadata")
        ),
        Err(message) => {
            println!(
                "{}",
                serde_json::json!({"ok":false,"diagnostics":[{"code":"VIEW_CONTRACT","message":message}]})
            );
            std::process::exit(1);
        }
    }
}
