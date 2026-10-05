use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Mutex;
use tokio::sync::oneshot;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorRequest {
    pub id: String,
    pub action: String,
    pub text: Option<String>,
    pub revision: Option<String>,
}
#[derive(Default)]
pub struct Editor {
    pending: Mutex<Option<(EditorRequest, oneshot::Sender<Value>)>>,
}
impl Editor {
    pub fn begin(
        &self,
        action: &str,
        text: Option<String>,
        revision: Option<String>,
    ) -> Result<(String, oneshot::Receiver<Value>), &'static str> {
        let mut pending = self.pending.lock().expect("editor request");
        if pending.is_some() {
            return Err("Another editor request is pending.");
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        *pending = Some((
            EditorRequest {
                id: id.clone(),
                action: action.into(),
                text,
                revision,
            },
            tx,
        ));
        Ok((id, rx))
    }
    pub fn peek(&self) -> Option<EditorRequest> {
        self.pending
            .lock()
            .expect("editor request")
            .as_ref()
            .map(|(r, _)| r.clone())
    }
    pub fn finish(&self, id: &str, result: Value) {
        let mut pending = self.pending.lock().expect("editor request");
        if pending.as_ref().is_some_and(|(r, _)| r.id == id) {
            let (_, tx) = pending.take().expect("pending");
            let _ = tx.send(result);
        }
    }
    pub fn abandon(&self, id: &str) {
        let mut pending = self.pending.lock().expect("editor request");
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
        let editor = Editor::default();
        let (id, reply) = editor.begin("read", None, None).unwrap();
        assert!(editor.begin("read", None, None).is_err());
        editor.finish("wrong-id", serde_json::json!({"ok":true}));
        assert_eq!(editor.peek().unwrap().id, id);
        editor.finish(&id, serde_json::json!({"ok":true,"revision":"one"}));
        assert_eq!(reply.await.unwrap()["revision"], "one");
        assert!(editor.peek().is_none());
        let (id, reply) = editor
            .begin("update", Some("draft".into()), Some("one".into()))
            .unwrap();
        editor.abandon(&id);
        assert!(reply.await.is_err());
        editor.finish(&id, serde_json::json!({"ok":true}));
        assert!(editor.peek().is_none());
    }
}
