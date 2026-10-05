//! Inert documentation, never invocation rules or permission evidence.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Note {
    pub kind: String,
    pub target: String,
    pub description: String,
    pub enforcement: String,
}

pub(crate) fn validate(value: &serde_json::Value) -> Result<Vec<Note>, &'static str> {
    let notes: Vec<Note> = serde_json::from_value(value.clone())
        .map_err(|_| "notes must be an array of constraint objects")?;
    if notes.len() > 1000
        || notes.iter().any(|n| {
            n.kind != "constraint"
                || n.enforcement != "not-checked-locally"
                || n.description.trim().is_empty()
                || n.description.len() > 2048
                || n.target.len() > 4096
                || !(n.target == "#" || n.target.starts_with("#/"))
        })
    {
        return Err("invalid or oversized constraint note");
    }
    Ok(notes)
}
