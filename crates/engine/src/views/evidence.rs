//! Explicit finite evidence uses ordinary result retention, never a second persistence store.
use super::*;
use wes_core::{Primitive, Provenance, RecordShape, Shape};
fn budget() -> u64 {
    wes_budgets::get("view.evidence.bytes") as u64
}
fn record(name: &str, fields: impl IntoIterator<Item = (String, Value)>) -> Value {
    let fields: Vec<_> = fields.into_iter().collect();
    let provenance = Provenance::agreed_by(fields.iter().map(|(_, v)| v.provenance()));
    let shape = Shape::Record(
        RecordShape::new(
            name,
            fields.iter().map(|(k, v)| (k.clone(), v.shape().clone())),
        )
        .expect("unique evidence fields"),
    );
    Value::new(
        shape,
        Data::Record(
            fields
                .into_iter()
                .map(|(k, v)| (k, v.data().clone()))
                .collect(),
        ),
        provenance,
    )
    .expect("typed evidence")
}
fn text(s: impl Into<Arc<str>>) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text(s.into()),
        Provenance::default(),
    )
    .unwrap()
}
fn boolean(b: bool) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Bool),
        Data::Bool(b),
        Provenance::default(),
    )
    .unwrap()
}
fn texts(items: impl IntoIterator<Item = String>) -> Value {
    Value::new(
        Shape::List(Box::new(Shape::Primitive(Primitive::Text))),
        Data::List(items.into_iter().map(|s| Data::Text(s.into())).collect()),
        Provenance::default(),
    )
    .unwrap()
}
fn charge(value: &Value, total: &mut u64) -> Result<(), String> {
    *total += crate::value_size::value_charge(value, budget())
        .ok_or("View evidence exceeds its 8 MiB budget")?;
    if *total > budget() {
        return Err("View evidence exceeds its 8 MiB budget".into());
    }
    Ok(())
}
impl crate::workspace::Workspace {
    pub(crate) fn capture_view(&self, node: &NodeId) -> Result<Value, String> {
        // Called synchronously at the workspace owner boundary: no awaits between validation,
        // linked-field resolution and the immutable snapshot.
        let frame = self.view_frame(node)?;
        let root = frame.instances.first().ok_or("View unavailable")?;
        let patches = self.view_input_patches(node, &root.identity)?;
        let mut total = 0;
        let mut entries = vec![];
        let mut complete = true;
        for entry in &frame.instances {
            let mut warnings = vec![];
            if let Some(problem) = &entry.input_problem {
                warnings.push(problem.clone());
            }
            if let Some(problem) = patches.problems.get(&entry.id) {
                warnings.push(problem.clone());
            }
            if entry.query_running {
                warnings.push(
                    "Query is running; captured data may precede the current selection".into(),
                );
            }
            let mut fields = vec![
                ("node".into(), text(entry.id.as_str())),
                ("instance".into(), text(entry.identity.clone())),
                (
                    "definition".into(),
                    text(entry.definition.manifest.name.as_str()),
                ),
                (
                    "packageDigest".into(),
                    text(entry.definition.digest.as_str()),
                ),
                (
                    "artifactDigest".into(),
                    Value::new(
                        Shape::Option(Box::new(Shape::Primitive(Primitive::Text))),
                        Data::Option(
                            entry
                                .definition
                                .artifact
                                .as_ref()
                                .map(|id| Box::new(Data::Text(id.as_str().into()))),
                        ),
                        Provenance::default(),
                    )
                    .expect("artifact identity"),
                ),
                ("revision".into(), text(entry.revision.to_string())),
                (
                    "inputRevision".into(),
                    text(entry.input_revision.to_string()),
                ),
                ("queryRunning".into(), boolean(entry.query_running)),
                (
                    "members".into(),
                    record(
                        "wes.ViewMembers",
                        entry.members.iter().map(|(slot, ids)| {
                            (
                                slot.clone(),
                                texts(ids.iter().map(|id| id.as_str().to_string())),
                            )
                        }),
                    ),
                ),
            ];
            if let Some(input) = entry.input.as_ref().and_then(|i| i.value.as_ref()) {
                charge(input, &mut total)?;
                warnings.extend(input.provenance().cautions().iter().cloned());
                let mut effective = input.clone();
                if let Some(overrides) = patches.values.get(&entry.id) {
                    let (Shape::Record(shape), Data::Record(data)) = (input.shape(), input.data())
                    else {
                        return Err("Expected record view input".into());
                    };
                    effective = record(
                        shape.name(),
                        data.iter()
                            .map(|(name, data)| {
                                let value = overrides.get(name).cloned().unwrap_or_else(|| {
                                    Value::new(
                                        shape.field(name).expect("validated field").clone(),
                                        data.clone(),
                                        input.provenance().clone(),
                                    )
                                    .expect("validated input field")
                                });
                                (name.clone(), value)
                            })
                            .chain(
                                overrides
                                    .iter()
                                    .filter(|(name, _)| !data.contains_key(*name))
                                    .map(|(name, value)| (name.clone(), value.clone())),
                            ),
                    );
                }
                fields.push((
                    "inputDigest".into(),
                    text(persistence::input_digest(&effective)),
                ));
                fields.push(("input".into(), effective));
                fields.push((
                    "queryEvidence".into(),
                    record(
                        "wes.QueryEvidence",
                        input
                            .provenance()
                            .facts()
                            .iter()
                            .filter(|(k, _)| k.starts_with("view.query."))
                            .map(|(k, v)| {
                                (
                                    k.trim_start_matches("view.query.").to_string(),
                                    text(v.as_str()),
                                )
                            }),
                    ),
                ));
            } else {
                warnings.push("Input unavailable; no source was rerun".into());
                complete = false;
            }
            if let Some(source) = entry.input.as_ref().and_then(|i| i.source()) {
                fields.push((
                    "source".into(),
                    record(
                        "wes.ViewSource",
                        [
                            ("node".into(), text(source.output.node.as_str())),
                            ("port".into(), text(format!("{:?}", source.output.port))),
                            (
                                "run".into(),
                                text(
                                    source
                                        .run
                                        .as_ref()
                                        .map(ToString::to_string)
                                        .unwrap_or_default(),
                                ),
                            ),
                            ("digest".into(), text(source.digest.as_str())),
                            ("fields".into(), texts(source.fields.clone())),
                        ],
                    ),
                ));
            }
            if let Some(items) = patches.cautions.get(&entry.id) {
                warnings.extend(items.iter().cloned());
            }
            let mut outputs = vec![];
            if entry.definition.manifest.interaction.is_some() {
                let (owner, state) = self.view_interaction(&entry.id, &entry.identity)?;
                fields.push(("stateOwner".into(), text(owner.identity.clone())));
                fields.push(("stateRevision".into(), text(state.revision.to_string())));
                let contract = owner
                    .definition
                    .contracts
                    .resolve(
                        &owner
                            .definition
                            .manifest
                            .interaction
                            .as_ref()
                            .expect("interaction")
                            .state,
                    )
                    .map_err(|e| e.to_string())?;
                let wes_core::contracts::ContractKind::Record(state_fields) = contract.kind()
                else {
                    return Err("Invalid interaction contract".into());
                };
                fields.push((
                    "interaction".into(),
                    record(
                        "wes.ViewInteraction",
                        state.fields.iter().map(|(name, data)| {
                            (
                                name.clone(),
                                Value::new(
                                    state_fields[name].contract.shape(),
                                    data.clone(),
                                    Provenance::default(),
                                )
                                .expect("validated shared state"),
                            )
                        }),
                    ),
                ));
                let handle = &self.views.instances[&entry.id].handle;
                for (name, port) in &owner.definition.manifest.outputs {
                    if !port.shared {
                        continue;
                    }
                    match self.views.output(handle, name) {
                        Ok(value) => {
                            charge(&value, &mut total)?;
                            warnings.extend(value.provenance().cautions().iter().cloned());
                            outputs.push((name.clone(), value));
                        }
                        Err(_) => warnings.push(format!("Output {name} is not initialized")),
                    }
                }
            }
            if entry.input_problem.is_some() || patches.problems.contains_key(&entry.id) {
                complete = false;
            }
            fields.push(("outputs".into(), record("wes.ViewOutputs", outputs)));
            warnings.sort();
            warnings.dedup();
            fields.push(("warnings".into(), texts(warnings)));
            entries.push((
                entry.id.as_str().to_string(),
                record("wes.ViewEvidenceEntry", fields),
            ));
        }
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?;
        let timestamp = wes_core::Timestamp::new(
            i64::try_from(time.as_secs()).map_err(|e| e.to_string())?,
            time.subsec_nanos(),
        )
        .map_err(|e| e.to_string())?;
        let result = record(
            "wes.ViewEvidence",
            [
                (
                    "capturedAt".into(),
                    Value::new(
                        Shape::Primitive(Primitive::Instant),
                        Data::Instant(timestamp),
                        Provenance::default(),
                    )
                    .unwrap(),
                ),
                ("root".into(), text(frame.root.as_str())),
                ("inputsComplete".into(), boolean(complete)),
                (
                    "retention".into(),
                    text(
                        "Ordinary result retention applies; check kept status or explicitly Keep/Pin this result.",
                    ),
                ),
                (
                    "consistency".into(),
                    text(
                        "One committed view snapshot; independent inputs may represent different source instants. Query evidence identifies the input used, not necessarily the current selection.",
                    ),
                ),
                ("entries".into(), record("wes.ViewEvidenceEntries", entries)),
            ],
        );
        charge(&result, &mut 0)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evidence_budget_is_aggregate_and_record_composition_cannot_drop_private_policy() {
        let value = text("x".repeat(1024));
        let mut total = 0;
        for _ in 0..9000 {
            if charge(&value, &mut total).is_err() {
                break;
            }
        }
        assert!(total > budget());
        let private = value.with_provenance(
            Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
        );
        let result = record(
            "Evidence",
            [
                ("data".into(), private),
                ("metadata".into(), text("public")),
            ],
        );
        assert!(result.provenance().policy().is_private());
    }
}
