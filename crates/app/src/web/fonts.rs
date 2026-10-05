//! `GET /fonts`: the font families installed on the machine the client draws on, each marked
//! monospaced or not, so `/settings` can offer data faces and interface faces from what exists.
//!
//! Read-only and local: it reads the platform's font catalogue and nothing else. Where there is no
//! catalogue to read, the answer is an empty list and the client offers the faces it ships.

use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Families answered at most; a machine with more is still offered the first ones in order.
fn max_families() -> usize {
    wes_budgets::get("ui.fonts") as usize
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Family {
    pub name: String,
    /// Every face of the family is monospaced.
    pub monospace: bool,
}

pub(super) async fn read() -> Response {
    let families = installed().await;
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        serde_json::json!({ "families": families }).to_string(),
    )
        .into_response()
}

/// One entry per family, sorted by name: a family is monospaced only when all its faces are, and
/// hidden system families (a leading `.`) and the last-resort face are left out.
pub(crate) fn families(faces: impl IntoIterator<Item = (String, bool)>) -> Vec<Family> {
    let mut by_name = std::collections::BTreeMap::<String, bool>::new();
    for (name, monospace) in faces {
        let name = name.trim().to_owned();
        if name.is_empty() || name.starts_with('.') || name == "LastResort" || name.len() > 200 {
            continue;
        }
        by_name
            .entry(name)
            .and_modify(|all| *all &= monospace)
            .or_insert(monospace);
    }
    by_name
        .into_iter()
        .take(max_families())
        .map(|(name, monospace)| Family { name, monospace })
        .collect()
}

/// AppKit's font manager, asked through JavaScript for Automation: a fixed script, no input, no
/// unsafe code in this crate. A family is fixed-pitch when every member carries
/// `NSFixedPitchFontMask` — the font's own metrics, so Monaco counts even where other catalogues
/// forget to mark it.
#[cfg(target_os = "macos")]
const CATALOGUE: &str = r#"ObjC.import("AppKit");
const manager = $.NSFontManager.sharedFontManager;
const out = [];
for (const name of ObjC.deepUnwrap(manager.availableFontFamilies) || []) {
  const members = ObjC.deepUnwrap(manager.availableMembersOfFontFamily(name)) || [];
  out.push([name, members.length > 0 && members.every(member => (member[3] & 0x400) !== 0)]);
}
JSON.stringify(out);"#;

/// How long the catalogue may take before the pickers fall back to the shipped faces.
#[cfg(target_os = "macos")]
const CATALOGUE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[cfg(target_os = "macos")]
pub(crate) async fn installed() -> Vec<Family> {
    let run = async {
        let child = wes_adapters::process::serialized_spawn_async(|| {
            tokio::process::Command::new("/usr/bin/osascript")
                .args(["-l", "JavaScript", "-e", CATALOGUE])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()
        })
        .await?;
        child.wait_with_output().await
    };
    let Ok(Ok(output)) = tokio::time::timeout(CATALOGUE_TIMEOUT, run).await else {
        tracing::warn!("font catalogue did not answer in time");
        return Vec::new();
    };
    if !output.status.success() {
        tracing::warn!(status = %output.status, "font catalogue failed");
        return Vec::new();
    }
    match serde_json::from_slice::<Vec<(String, bool)>>(&output.stdout) {
        Ok(faces) => families(faces),
        Err(error) => {
            tracing::warn!(%error, "font catalogue answered unreadable output");
            Vec::new()
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) async fn installed() -> Vec<Family> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_keep_one_sorted_entry_per_family_without_hidden_faces_when_faces_are_listed() {
        // Arrange
        let faces = [
            ("Menlo", true),
            ("Avenir Next", false),
            ("Menlo", true),
            (".SF NS Mono", true),
            ("LastResort", false),
            ("Mixed", true),
            ("Mixed", false),
        ];
        // Act
        let listed = families(faces.map(|(name, mono)| (name.to_owned(), mono)));
        // Assert
        assert_eq!(
            listed,
            vec![
                Family {
                    name: "Avenir Next".into(),
                    monospace: false
                },
                Family {
                    name: "Menlo".into(),
                    monospace: true
                },
                Family {
                    name: "Mixed".into(),
                    monospace: false
                },
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn should_find_menlo_monospaced_and_helvetica_not_when_the_mac_catalogue_is_read() {
        // Act: families every macOS installation ships.
        let listed = installed().await;
        let find = |name: &str| listed.iter().find(|family| family.name == name).cloned();
        // Assert
        assert_eq!(find("Menlo").map(|family| family.monospace), Some(true));
        assert_eq!(
            find("Helvetica").map(|family| family.monospace),
            Some(false)
        );
        assert!(listed.iter().all(|family| !family.name.starts_with('.')));
    }
}
