//! Client-facing explanations of inert reconstruction evidence; never execution authority.
use wes_engine::{
    graph::NodeId,
    session::{RestoreProblemKind, RestoreReport},
    storage::ValueHandle,
};

pub struct Warning {
    pub id: String,
    pub message: String,
    pub node: Option<NodeId>,
    pub handle: Option<ValueHandle>,
}
pub fn warnings(report: &RestoreReport) -> Vec<Warning> {
    let mut warnings = vec![];
    for (id, count, message) in [
        (
            "unidentified-cells",
            report.unidentified_cells,
            "submission identities have no recoverable source and cannot be retried as new submissions",
        ),
        (
            "unconfirmed-changes",
            report.unconfirmed_external_changes.len(),
            "changed declarations have no confirmed subsequent execution and remain stale",
        ),
        (
            "interrupted",
            report.interrupted.len(),
            "calls have no matching completion record; their external outcome is unknown and they were not repeated",
        ),
    ] {
        if count != 0 {
            warnings.push(Warning {
                id: id.into(),
                message: if id == "unconfirmed-changes" {
                    format!(
                        "{count} {} stale after changes; no later execution is confirmed.",
                        if count == 1 {
                            "provider-capable declaration remains"
                        } else {
                            "provider-capable declarations remain"
                        }
                    )
                } else {
                    format!("{count} {message}.")
                },
                node: None,
                handle: None,
            });
        }
    }
    for (index, problem) in report.problems.iter().enumerate() {
        let message = match problem.kind {
            RestoreProblemKind::MissingValue => {
                "The retained value was unavailable when this workspace opened."
            }
            RestoreProblemKind::UnreadableValue => {
                "The retained value could not be read when this workspace opened."
            }
            RestoreProblemKind::NoValueStore => {
                "No result store was configured when this workspace opened."
            }
            RestoreProblemKind::ValueCapacity => {
                "The retained value exceeded the reconstruction budget when this workspace opened."
            }
        };
        warnings.push(Warning {
            id: format!("value-{index}"),
            message: format!("{message} The command was not repeated."),
            node: Some(problem.node.clone()),
            handle: Some(problem.handle.clone()),
        });
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_engine::session::RestoreProblem;
    #[test]
    fn startup_explanations_preserve_missing_value_identity_without_claiming_remote_failure() {
        assert!(warnings(&RestoreReport::default()).is_empty());
        let handle = ValueHandle::fresh();
        let report = RestoreReport {
            unidentified_cells: 1,
            problems: vec![RestoreProblem {
                node: NodeId::new("n").unwrap(),
                handle: handle.clone(),
                kind: RestoreProblemKind::MissingValue,
            }],
            ..Default::default()
        };
        let messages = warnings(&report);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].node.as_ref().unwrap().as_str(), "n");
        assert_eq!(messages[1].handle.as_ref(), Some(&handle));
        assert!(messages[1].message.contains("was not repeated"));
        assert_ne!(messages[0].id, messages[1].id);
    }
}

/// One-shot CLI commands already report stale reads/inspection directly. Do not repeat the
/// declaration summary on unrelated invocations. Actual interrupted attempts and missing
/// retained values remain visible; this is presentation, never recovery acknowledgement.
pub fn command_warnings(report: &RestoreReport) -> impl Iterator<Item = Warning> {
    warnings(report)
        .into_iter()
        .filter(|warning| warning.id != "unconfirmed-changes")
}

#[cfg(test)]
mod retrospective_tests {
    use super::*;
    #[test]
    fn pure_stale_nodes_are_not_external_warnings_and_cli_does_not_acknowledge_evidence() {
        let report = RestoreReport {
            unconfirmed_changes: vec![NodeId::new("pure").unwrap()],
            ..Default::default()
        };
        assert!(warnings(&report).is_empty());
        let report = RestoreReport {
            unconfirmed_changes: vec![
                NodeId::new("pure").unwrap(),
                NodeId::new("provider").unwrap(),
            ],
            unconfirmed_external_changes: vec![NodeId::new("provider").unwrap()],
            unidentified_cells: 1,
            ..Default::default()
        };
        let all = warnings(&report);
        assert_eq!(all.len(), 2);
        assert!(
            all[1]
                .message
                .contains("1 provider-capable declaration remains")
        );
        let cli: Vec<_> = command_warnings(&report).collect();
        assert_eq!(cli.len(), 1);
        assert_eq!(cli[0].id, "unidentified-cells");
        assert_eq!(report.unconfirmed_changes.len(), 2);
        assert_eq!(report.unconfirmed_external_changes.len(), 1);
    }
}
