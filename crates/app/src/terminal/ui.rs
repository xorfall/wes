//! Typed UI request brokerage, independent of MCP and code editor implementation.
pub(crate) mod render;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Mutex;
use tokio::sync::oneshot;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: String,
    pub operation: Operation,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    DraftRead,
    DraftUpdate {
        text: String,
        revision: String,
    },
    LayoutRead,
    TabOpen {
        workspace: String,
        pane: String,
        activate: bool,
    },
    ViewRenderStatus {
        scope: RenderScope,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderScope {
    pub workspace: String,
    pub generation: String,
    pub node: String,
    pub instance: String,
}
#[derive(Default)]
pub struct Broker {
    pending: Mutex<Option<(Request, oneshot::Sender<Value>)>>,
}
impl Broker {
    pub fn begin(
        &self,
        operation: Operation,
    ) -> Result<(String, oneshot::Receiver<Value>), &'static str> {
        let mut pending = self.pending.lock().expect("UI request");
        if pending.is_some() {
            return Err("Another UI request is pending.");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        *pending = Some((
            Request {
                id: id.clone(),
                operation,
            },
            tx,
        ));
        Ok((id, rx))
    }
    pub fn peek(&self) -> Option<Request> {
        self.pending
            .lock()
            .expect("UI request")
            .as_ref()
            .map(|(r, _)| r.clone())
    }
    pub fn finish(&self, id: &str, result: Value) {
        let mut pending = self.pending.lock().expect("UI request");
        if pending.as_ref().is_some_and(|(r, _)| r.id == id) {
            let (_, tx) = pending.take().expect("pending");
            let _ = tx.send(result);
        }
    }
    pub fn abandon(&self, id: &str) {
        let mut pending = self.pending.lock().expect("UI request");
        if pending.as_ref().is_some_and(|(r, _)| r.id == id) {
            pending.take();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn editor_requests_match_acknowledgements_and_expire_without_reuse() {
        let editor = Broker::default();
        let (id, reply) = editor.begin(Operation::DraftRead).unwrap();
        assert!(editor.begin(Operation::DraftRead).is_err());
        editor.finish("wrong-id", serde_json::json!({"ok":true}));
        assert_eq!(editor.peek().unwrap().id, id);
        editor.finish(&id, serde_json::json!({"ok":true,"revision":"one"}));
        assert_eq!(reply.await.unwrap()["revision"], "one");
        assert!(editor.peek().is_none());
        let (id, reply) = editor
            .begin(Operation::DraftUpdate {
                text: "draft".into(),
                revision: "one".into(),
            })
            .unwrap();
        editor.abandon(&id);
        assert!(reply.await.is_err());
        editor.finish(&id, serde_json::json!({"ok":true}));
        assert!(editor.peek().is_none());
    }
}
