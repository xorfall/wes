use serde_json::{Value, json};

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
