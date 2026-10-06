use serde_json::{Value, json};

/// Operation semantics, not inferred/private output metadata. Reading source is required
/// because even the operation kind is withheld from a reader without source permission.
pub(crate) fn public_completion(
    observation: &wes_engine::session::SessionObservation,
    node: &wes_engine::graph::NodeId,
    source_permitted: bool,
) -> Option<Value> {
    if !source_permitted {
        return None;
    }
    let notice = observation.completion_notices.get(node)?;
    Some(
        json!({"code":notice.code,"message":notice.message,"resultAccess":notice.result_access,"grantsChanged":notice.grants_changed}),
    )
}

/// Shared bounded metadata for GUI and agent projections; no values or authority tokens.
pub(crate) fn waiting_inputs<'a>(
    items: &[wes_engine::runtime::WaitingInput],
    names: impl Iterator<Item = (&'a String, &'a wes_engine::graph::OutputRef)> + Clone,
) -> Value {
    json!(items.iter().map(|item|json!({"source":item.source.as_str(),"port":item.port_name(),"state":if item.closed {"closed"}else{"pending"},"run":item.run.as_ref().map(ToString::to_string),"message":item.message_with_name(names.clone().find(|(_,output)|output.node == item.source && output.port == wes_engine::graph::OutputPort::Data).map(|(name,_)|name.as_str()))})).collect::<Vec<_>>())
}

pub(crate) fn operation_receipt(receipt: &wes_engine::workspace::OperationReceipt) -> Value {
    json!({"operation":receipt.operation,"target":receipt.target,"node":receipt.node.as_ref().map(|node|node.as_str()),"requested":receipt.requested,"started":receipt.started,"stale":receipt.stale,"skipped":receipt.skipped,"removed":receipt.removed,"unbound":receipt.unbound,"detail":receipt.detail,"summary":receipt.summary()})
}
