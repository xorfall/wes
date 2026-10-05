//! Immutable compiled view artifacts. Parsing grants no execution authority.
use crate::Package;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The compiler, MCP discovery and native gate share the supported frame protocol version.
pub fn sdk_version() -> u8 {
    static VERSION: std::sync::OnceLock<u8> = std::sync::OnceLock::new();
    *VERSION.get_or_init(|| {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../../packages/view-sdk/authoring.json"))
                .expect("View authoring manifest");
        u8::try_from(manifest["sdk"].as_u64().expect("View SDK version"))
            .expect("bounded View SDK version")
    })
}

pub const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
const MAX_RENDERER_BYTES: usize = 4 * 1024 * 1024;
const MAX_STYLE_BYTES: usize = 256 * 1024;

/// A self-contained package: no executable paths, URLs or unresolved dependencies.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSource {
    pub format: u8,
    pub sdk: u8,
    pub manifest: String,
    pub types: String,
    pub definition: String,
    pub javascript: String,
    pub css: String,
}

#[derive(Clone, Debug)]
pub struct Artifact {
    pub source: ArtifactSource,
    pub package: Package,
    pub digest: String,
}

impl Artifact {
    pub fn recognizes(source: &str) -> bool {
        source.trim_start().starts_with('{')
            && serde_json::from_str::<serde_json::Value>(source)
                .ok()
                .is_some_and(|value| value.get("sdk").is_some())
    }
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err("View artifact exceeds the 8 MiB limit".into());
        }
        let source: ArtifactSource = serde_json::from_slice(bytes)
            .map_err(|e| format!("Invalid compiled view artifact: {e}"))?;
        if source.format != 1 {
            return Err("Unsupported view artifact format".into());
        }
        if source.sdk != sdk_version() {
            return Err(format!(
                "View uses SDK {}; this Wes requires SDK {}. Rebuild the source package with the current wes-view-package compiler.",
                source.sdk,
                sdk_version()
            ));
        }
        if source.javascript.is_empty() || source.javascript.len() > MAX_RENDERER_BYTES {
            return Err("View renderer must contain 1 byte..4 MiB of compiled code".into());
        }
        if source.css.len() > MAX_STYLE_BYTES {
            return Err("View CSS exceeds the 256 KiB limit".into());
        }
        if source.javascript.to_ascii_lowercase().contains("</script")
            || source.css.to_ascii_lowercase().contains("</style")
        {
            return Err("View assets contain an unsafe HTML raw-text terminator".into());
        }
        let mut package = Package::parse(&source.manifest, &source.types)?;
        if source.definition != package.digest {
            return Err("Compiled view definition does not match its contracts".into());
        }
        // Definition identity is semantic. Artifact identity additionally pins every byte of
        // renderer and CSS, so a saved instance never silently executes replacement code.
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&source).map_err(|e| e.to_string())?)
        );
        package.artifact = Some(digest.clone());
        Ok(Self {
            source,
            package,
            digest,
        })
    }
}
