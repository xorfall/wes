use super::*;
use crate::history::{JournalEntry, Record, SavedSelection};
use sha2::{Digest, Sha256};

impl Sandboxes {
    pub(super) async fn keep(
        &self,
        state: &mut State,
        input: &SourceInput,
        reference: &str,
        saved_name: &str,
    ) -> Result<Reply, SessionError> {
        let (name, member) = reference
            .split_once('.')
            .map_or((reference, None), |(n, m)| (n, Some(m)));
        if !wes_language::binding_name(saved_name) || saved_name.len() > 128 {
            return Err(SessionError::CommandArgument(
                "Use a valid saved selection name.",
            ));
        }
        if self.names().iter().any(|n| n == saved_name) {
            return Err(error(
                "The saved name already belongs to a workspace object.",
            ));
        }
        if state.saved.len() + state.pending_saves.len()
            >= wes_budgets::get("values.outputs") as usize
        {
            return Err(SessionError::Capacity);
        }
        let (recorder, required) = self.recording.as_ref().ok_or(SessionError::NoHistory)?;
        let recorder = recorder.upgrade().ok_or(SessionError::Stopped)?;
        self.names
            .lock()
            .expect("program names")
            .reserved
            .insert(saved_name.into());
        let _reservation = NameReservation {
            names: &self.names,
            name: saved_name.into(),
        };
        let _ = self.capture(input, Some(saved_name.into()), None).await?;
        let running = state.running.get(name).ok_or_else(|| {
            error("The transient analysis has no live result. Keep never starts or refreshes it.")
        })?;
        let worker = running.storage.as_ref().ok_or(SessionError::NoStorage)?;
        let (reply, receive) = oneshot::channel();
        running
            .handle
            .controls
            .send(Control::SavedCapture {
                member: member.map(str::to_owned),
                reply,
            })
            .await
            .map_err(|_| SessionError::Stopped)?;
        let mut capture = receive.await.map_err(|_| SessionError::Stopped)??;
        capture.protect(worker).await?;
        let single = member.is_some();
        let alias_shape = if single {
            capture.shapes.get(&capture.members[0].name).cloned()
        } else {
            Some(wes_core::Shape::Record(
                wes_core::RecordShape::new(
                    "Record",
                    capture
                        .shapes
                        .iter()
                        .map(|(n, shape)| (n.clone(), shape.clone())),
                )
                .map_err(|_| SessionError::Preparation)?,
            ))
        };
        let saved = SavedSelection {
            id: uuid::Uuid::new_v4().to_string(),
            name: saved_name.into(),
            owner: input.client().into(),
            scope: capture.scope,
            definition: format!(
                "sha256:{:x}",
                Sha256::digest(state.definitions[name].source.as_bytes())
            ),
            single,
            alias_shape,
            members: capture.members,
            lineage: capture.lineage,
        };
        saved.validate().map_err(|_| SessionError::Preparation)?;
        let final_record = Arc::new(Record::Journal(JournalEntry::Saved(saved.clone())));
        let intent_record = Arc::new(Record::Journal(JournalEntry::SaveIntent(saved.clone())));
        recorder
            .check_record(&final_record)
            .map_err(|_| SessionError::Capacity)?;
        state.pending_saves.insert(saved_name.into(), saved.clone());
        let intent = recorder.append(intent_record).await;
        if !intent.is_ok_and(|receipt| required.accepts(receipt.persistence)) {
            return Err(SessionError::KeepIncomplete {
                identity: saved.id,
                stage: "selection intent history was not acknowledged",
                may_have_applied: true,
            });
        }
        for handle in saved.handles() {
            let receipt =
                running
                    .handle
                    .keep(handle)
                    .await
                    .map_err(|_| SessionError::KeepIncomplete {
                        identity: saved.id.clone(),
                        stage: "payload retention was not acknowledged",
                        may_have_applied: true,
                    })?;
            if receipt.problem.is_some()
                || !receipt
                    .stored
                    .as_ref()
                    .is_some_and(|s| s.kept && required.accepts(s.retained_persistence))
            {
                return Err(SessionError::KeepIncomplete {
                    identity: saved.id,
                    stage: "a required payload or dependency was not durably retained",
                    may_have_applied: true,
                });
            }
        }
        let ack = recorder.append(final_record).await;
        if !ack.is_ok_and(|receipt| required.accepts(receipt.persistence)) {
            return Err(SessionError::KeepIncomplete {
                identity: saved.id,
                stage: "final saved-record history was not acknowledged",
                may_have_applied: true,
            });
        }
        state.pending_saves.remove(saved_name);
        state.saved.insert(saved_name.into(), saved.clone());
        let stored = if capture.values.len()
            != saved.members.iter().filter(|m| m.payload.is_some()).count()
        {
            super::super::saved::stored_alias(
                self.storage.as_ref().ok_or(SessionError::NoStorage)?,
                &saved,
            )
            .await
            .map_err(|_| SessionError::KeepIncomplete {
                identity: saved.id.clone(),
                stage: "retained snapshot is recorded; stored alias preparation failed",
                may_have_applied: false,
            })?
        } else {
            None
        };
        let (reply, receive) = oneshot::channel();
        let installed = match self.controls.upgrade() {
            Some(controls) => {
                controls
                    .send(Control::SavedInstall {
                        saved: saved.clone(),
                        values: capture.values,
                        stored,
                        reply,
                    })
                    .await
                    .is_ok()
                    && receive.await.is_ok_and(|r| r.is_ok())
            }
            None => false,
        };
        drop(capture.holds);
        if !installed {
            return Err(SessionError::KeepIncomplete {
                identity: saved.id,
                stage: "retained snapshot is recorded; use read/inspect because its live binding was not installed",
                may_have_applied: false,
            });
        }
        self.read_saved(&saved, saved_name, true, false).await
    }
    pub(super) async fn read_saved(
        &self,
        saved: &SavedSelection,
        reference: &str,
        inspect: bool,
        cooperative: bool,
    ) -> Result<Reply, SessionError> {
        let suffix = reference
            .strip_prefix(&saved.name)
            .and_then(|s| s.strip_prefix('.'));
        let worker = self.storage.as_ref().ok_or(SessionError::NoStorage)?;
        let mut withheld = std::collections::BTreeSet::new();
        if cooperative {
            for member in &saved.members {
                if !saved.single
                    && suffix.is_some_and(|s| s.split('.').next() != Some(&member.name))
                {
                    continue;
                }
                if let Some(payload) = &member.payload {
                    // A child projection never relaxes the captured root's export policy.
                    let snapshot = worker
                        .describe(
                            crate::storage::ValueHandle::new(&payload.handle)
                                .map_err(super::super::saved::store_error)?,
                            vec![],
                        )
                        .await
                        .map_err(super::super::saved::store_error)?
                        .ok_or(SessionError::UnknownValue)?;
                    if snapshot.digest != payload.digest {
                        return Err(super::super::saved::store_error(
                            crate::storage::StoreError::Conflict,
                        ));
                    }
                    let policy = snapshot.provenance.policy();
                    if policy.is_confidential() || policy.is_unknown() {
                        if !inspect {
                            return Err(SessionError::Authority);
                        }
                        withheld.insert(member.name.as_str());
                    }
                }
            }
        }
        let mut loaded_values = BTreeMap::new();
        let data = if !inspect && saved.single {
            let fields =
                suffix.map_or_else(Vec::new, |s| s.split('.').map(str::to_owned).collect());
            let value =
                super::super::saved::load_member(worker, &saved.members[0], &fields).await?;
            loaded_values.insert(String::new(), value.clone());
            value.data().clone()
        } else if !inspect && suffix.is_some() {
            let mut parts = suffix.unwrap().split('.');
            let name = parts.next().unwrap();
            let member = saved
                .members
                .iter()
                .find(|m| m.name == name)
                .ok_or(SessionError::UnknownValue)?;
            let value = super::super::saved::load_member(
                worker,
                member,
                &parts.map(str::to_owned).collect::<Vec<_>>(),
            )
            .await?;
            loaded_values.insert(String::new(), value.clone());
            value.data().clone()
        } else {
            let selected = if saved.single {
                None
            } else {
                suffix.and_then(|s| s.split('.').next())
            };
            let mut rows = Vec::new();
            let mut remaining = 4 * 1024 * 1024;
            for member in &saved.members {
                if selected.is_some_and(|s| s != member.name) {
                    continue;
                }
                if withheld.contains(member.name.as_str()) {
                    rows.push(record([
                        ("name", text(&member.name)),
                        ("state", text("withheld")),
                    ]));
                    continue;
                }
                let mut row = indexmap::IndexMap::from([
                    ("name".into(), text(&member.name)),
                    ("type".into(), text(&member.typing)),
                    ("state".into(), text(&member.state)),
                    ("node".into(), text(&member.node)),
                    ("revision".into(), text(member.revision.to_string())),
                ]);
                row.insert(
                    "retainedReference".into(),
                    Data::Bool(member.payload.is_some()),
                );
                row.insert("availability".into(), text("not checked"));
                if let Some(evidence) = &member.evidence {
                    row.insert("evidence".into(), text(evidence));
                }
                if !inspect && member.payload.is_some() {
                    match super::super::saved::load_member(worker, member, &[]).await {
                        Ok(value) => {
                            if let Some(charge) = crate::value_size::value_charge(&value, remaining)
                            {
                                remaining -= charge;
                                row.insert("value".into(), value.data().clone());
                                loaded_values.insert(member.name.clone(), value);
                            } else {
                                row.insert(
                                    "availability".into(),
                                    text("stored; decoded capacity required"),
                                );
                            }
                        }
                        Err(error)
                            if error.read_failure_kind()
                                == Some(crate::storage::ReadFailureKind::Capacity) =>
                        {
                            row.insert(
                                "availability".into(),
                                text("stored; decoded capacity required"),
                            );
                        }
                        Err(error) => return Err(error),
                    }
                }
                rows.push(Data::Record(row));
            }
            if selected.is_some() && rows.is_empty() {
                return Err(SessionError::UnknownValue);
            }
            record([
                ("kind", text("Saved")),
                ("identity", text(&saved.id)),
                ("persistence", text("retained snapshot")),
                ("state", text("saved")),
                ("members", Data::List(rows)),
            ])
        };
        crate::value_size::data_charge(&data, 4 * 1024 * 1024).ok_or(SessionError::Capacity)?;
        let mut sources = BTreeMap::new();
        if !inspect {
            for member in &saved.members {
                if let Some(payload) = &member.payload {
                    let mut fields = member.fields.clone();
                    let key = if saved.single {
                        fields.extend(
                            suffix
                                .into_iter()
                                .flat_map(|s| s.split('.'))
                                .map(str::to_owned),
                        );
                        String::new()
                    } else if let Some(suffix) = suffix {
                        let mut parts = suffix.split('.');
                        if parts.next() != Some(member.name.as_str()) {
                            continue;
                        }
                        fields.extend(parts.map(str::to_owned));
                        String::new()
                    } else {
                        member.name.clone()
                    };
                    if let Some(value) = loaded_values.get(&key) {
                        sources.insert(
                            key,
                            StoredSource {
                                handle: Some(payload.handle.clone()),
                                scope: None,
                                select: selection_path(&fields),
                                node: member.node.clone(),
                                run: member.run.clone(),
                                revision: member.revision.to_string(),
                                evidence: member.evidence.clone(),
                                shape: value.shape().clone(),
                                metadata: value.metadata().cloned(),
                                provenance: value.provenance().clone(),
                            },
                        );
                    }
                }
            }
        }
        Ok(Reply {
            kind: Kind::Saved,
            scope: None,
            sources,
            name: saved.name.clone(),
            data,
            state: ObservationState::Saved,
            view: Some((reference.into(), inspect)),
        })
    }
}
