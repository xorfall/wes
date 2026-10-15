//! Shared original source/recording admission for streams and finite companions.
use crate::{
    driver::ExecutionReport,
    eventlog::owner::Owner,
    graph::NodeId,
    providers::{OwnedSource, PreparedSource},
    runtime::Run,
};

pub(super) async fn start(
    owner: Owner,
    run: Run,
    admission: PreparedSource,
    required_setup: Option<(NodeId, String)>,
) -> Result<OwnedSource, ExecutionReport> {
    let source_reservation = owner
        .reserve_source_with_recording(&run, admission.records_bytes())
        .map_err(|message| admission.refusal(message))?;
    let claims = match admission.recording_admission() {
        Some((definition, _)) => {
            owner.claim_start(run.node(), &definition, required_setup.as_ref())
        }
        None if required_setup.is_some() => {
            Err("Recording launch has no workspace-captured source definition".into())
        }
        None => Ok(Vec::new()),
    }
    .map_err(|message| admission.refusal(message))?;
    let policy = admission
        .recording_admission()
        .map(|(_, policy)| policy)
        .unwrap_or_default();
    let mut prepared = Vec::new();
    let mut preparation_failure = None;
    let mut claims = claims.into_iter();
    while let Some(claim) = claims.next() {
        match claim.prepare_writer(run.id().to_string(), &policy).await {
            Ok(writer) => prepared.push((claim, writer)),
            Err(message) => {
                let required = claim.required;
                claim.complete(Err(message.clone()));
                if required {
                    for unentered in claims.by_ref() {
                        unentered.complete(Err(message.clone()));
                    }
                    preparation_failure = Some(message);
                    break;
                }
            }
        }
    }
    let mut admitted = Vec::new();
    for (claim, writer) in prepared {
        if claim.cancelled() {
            if claim.required {
                preparation_failure =
                    Some("Recording setup was cancelled before producer admission".into());
            }
            claim.reject(
                writer,
                "Recording was cancelled during preparation; this does not cancel its source"
                    .into(),
            );
        } else {
            admitted.push((claim, writer));
        }
    }
    let prepared = admitted;
    if let Some(message) = preparation_failure {
        for (claim, writer) in prepared {
            claim.reject(writer, message.clone());
        }
        return Err(admission.refusal(message));
    }
    let archives = prepared
        .iter()
        .map(|(_, (writer, _))| writer.receiver.branch.clone())
        .collect();
    let source = match admission.start_owned_with_archives(archives) {
        Ok(started) => started,
        Err(report) => {
            for (claim, writer) in prepared {
                claim.reject(
                    writer,
                    "Producer admission was refused before capture".into(),
                );
            }
            return Err(report);
        }
    };
    for (claim, writer) in prepared {
        claim.complete(Ok(writer));
    }
    source_reservation.install(source.handle().clone());
    Ok(source.after_join(move || owner.remove_source(&run)))
}
