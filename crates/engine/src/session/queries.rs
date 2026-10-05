//! Session-owned metadata capture, after run entry and before worker dispatch.
use super::Actor;
use crate::{
    runtime::RunTicket,
    tasks::{BoundTask, query::SessionQuery},
};
use wes_core::Data;
impl Actor {
    pub(super) fn capture_session_query(&self, ticket: &mut RunTicket<BoundTask>) {
        let BoundTask::Query(query) = &mut ticket.payload else {
            return;
        };
        let Some(request) = query.session_request() else {
            return;
        };
        let cells = self.cells.observed();
        let row = |cell: &super::ObservedCell| {
            let (status, nodes) = match &cell.reply {
                None => ("pending", vec![]),
                Some(Err(_)) => ("failed", vec![]),
                Some(Ok(reply)) => (
                    if reply
                        .diagnostics
                        .diagnostics
                        .iter()
                        .any(|d| d.severity == wes_language::Severity::Error)
                    {
                        "failed"
                    } else {
                        "complete"
                    },
                    reply
                        .nodes
                        .iter()
                        .map(|n| Data::Text(n.as_str().into()))
                        .collect(),
                ),
            };
            Data::Record(
                [
                    ("id".into(), Data::Text(cell.input.cell().into())),
                    ("status".into(), Data::Text(status.into())),
                    ("nodes".into(), Data::List(nodes)),
                    (
                        "sourceBytes".into(),
                        Data::Int(cell.input.text().len() as i64),
                    ),
                ]
                .into_iter()
                .collect(),
            )
        };
        let data = match request {
            SessionQuery::Sandboxes => Ok(Data::List(
                self.sandboxes
                    .definition_names()
                    .into_iter()
                    .map(|name| Data::Text(name.into()))
                    .collect(),
            )),
            SessionQuery::Cells => Ok(Data::List(cells.iter().map(row).collect())),
            SessionQuery::Cell(id) => cells
                .iter()
                .find(|c| c.input.cell() == id)
                .map(row)
                .ok_or("Cell is not present in this session."),
        };
        query.capture_session(data);
    }
}
