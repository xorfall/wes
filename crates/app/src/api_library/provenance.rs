//! Definition evidence belongs to immutable spec revisions, never runtime authority.
use super::*;

fn update(bytes: &[u8], edit: impl FnOnce(&mut Value)) -> Result<Vec<u8>> {
    // Validate the original bytes before serde transforms them (duplicate keys/budgets).
    validate_descriptor(bytes)?;
    let mut descriptor: Value = serde_json::from_slice(bytes).map_err(std::io::Error::other)?;
    let original = descriptor.clone();
    edit(&mut descriptor);
    if descriptor == original {
        return Ok(bytes.to_vec());
    }
    // arbitrary_precision preserves schema numeric literals while metadata is edited.
    let result = serde_json::to_vec_pretty(&descriptor).map_err(std::io::Error::other)?;
    validate_descriptor(&result)?;
    Ok(result)
}

pub(super) fn located(bytes: &[u8], location: &str) -> Result<Vec<u8>> {
    update(bytes, |descriptor| {
        if let Some(source) = descriptor.get_mut("source").and_then(Value::as_object_mut) {
            source.insert(
                "location".into(),
                json!(io_layer::portable_source_location(location)),
            );
        }
    })
}

pub(super) fn edited(bytes: &[u8]) -> Result<Vec<u8>> {
    update(bytes, |descriptor| {
        if let Some(provenance) = descriptor
            .pointer_mut("/source/provenance")
            .and_then(Value::as_object_mut)
        {
            provenance.insert("status".into(), json!("stale"));
        }
    })
}
