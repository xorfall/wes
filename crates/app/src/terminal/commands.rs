//! Single-flight pane requests. Claim before applying: an uncertain delivery is never replayed.
use super::*;
use tokio::sync::oneshot;
#[derive(Clone, Serialize)]
pub struct CommandRequest {
    pub id: String,
    pub text: String,
    // Absent for unmanaged sessions; null means an explicitly unselected context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<Option<String>>,
}
struct Pending {
    request: CommandRequest,
    claimed: bool,
    reply: oneshot::Sender<Option<String>>,
}
#[derive(Default)]
pub(super) struct Commands(Mutex<Option<Pending>>);
impl Commands {
    fn begin(
        &self,
        text: String,
        environment: Option<Option<String>>,
    ) -> Result<(String, oneshot::Receiver<Option<String>>), String> {
        let mut pending = self.0.lock().expect("pane command");
        if pending.is_some() {
            return Err("Another pane command is pending.".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (reply, rx) = oneshot::channel();
        *pending = Some(Pending {
            request: CommandRequest {
                id: id.clone(),
                text,
                environment,
            },
            claimed: false,
            reply,
        });
        Ok((id, rx))
    }
    pub fn peek(&self) -> Option<CommandRequest> {
        self.0
            .lock()
            .expect("pane command")
            .as_ref()
            .filter(|p| !p.claimed)
            .map(|p| p.request.clone())
    }
    pub fn claim(&self, id: &str) -> bool {
        let mut pending = self.0.lock().expect("pane command");
        if let Some(p) = pending
            .as_mut()
            .filter(|p| p.request.id == id && !p.claimed)
        {
            p.claimed = true;
            true
        } else {
            false
        }
    }
    pub fn finish(&self, id: &str, error: Option<String>) {
        let mut pending = self.0.lock().expect("pane command");
        if pending
            .as_ref()
            .is_some_and(|p| p.request.id == id && p.claimed)
        {
            let _ = pending.take().expect("pending").reply.send(error);
        }
    }
    fn abandon(&self, id: &str) {
        let mut pending = self.0.lock().expect("pane command");
        if pending.as_ref().is_some_and(|p| p.request.id == id) {
            pending.take();
        }
    }
}
pub(super) async fn dispatch(
    terminal: &TerminalSession,
    text: String,
    actor: Option<&str>,
) -> Result<String, BridgeReply> {
    if text.len() > 1024 || text.chars().any(char::is_control) || !text.starts_with('/') {
        return Err(BridgeReply::error(
            2,
            "Use wesx --cmd with one quoted pane /command (up to 1024 bytes).",
        ));
    }
    let observation = match actor {
        Some(actor) => {
            terminal
                .current
                .session
                .observe_actor_in(
                    actor.to_owned(),
                    terminal.client.clone(),
                    terminal.environment.clone(),
                )
                .await
        }
        None => terminal.current.session.observe().await,
    }
    .map_err(|_| BridgeReply::error(3, "Workspace context is unavailable."))?;
    let environment = match actor {
        Some(actor) => bridge::context(&observation, actor),
        None => bridge::terminal_context(&observation, terminal),
    }
    .map(|context| context.selected);
    let (id, reply) = terminal
        .commands
        .begin(text, environment)
        .map_err(|e| BridgeReply::error(2, e))?;
    terminal.changed.notify_waiters();
    let result = tokio::select! {
        result = reply => match result {
            Ok(None) => Ok(String::new()),
            Ok(Some(error)) => Err(BridgeReply::error(2, error)),
            Err(_) => Err(BridgeReply::error(3, "Pane command was cancelled.")),
        },
        _ = terminal.stopped.cancelled() => Err(BridgeReply::error(3, "Terminal ended.")),
        _ = tokio::time::sleep(Duration::from_secs(10)) => Err(BridgeReply::error(3,
            "Pane command acknowledgement timed out. It may have been applied; inspect the layout before repeating. No retry was made.")),
    };
    terminal.commands.abandon(&id);
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn claims_are_single_use_and_expired_commands_cannot_be_applied() {
        let commands = Commands::default();
        let (id, reply) = commands
            .begin("/split".into(), Some(Some("DEV".into())))
            .unwrap();
        assert!(commands.begin("/close".into(), None).is_err());
        commands.finish(&id, None); // An unclaimed request cannot be acknowledged.
        assert_eq!(
            commands.peek().unwrap().environment,
            Some(Some("DEV".into()))
        );
        assert!(!commands.claim("wrong"));
        assert!(commands.claim(&id));
        assert!(!commands.claim(&id));
        assert!(commands.peek().is_none());
        commands.finish(&id, Some("Four panes".into()));
        assert_eq!(reply.await.unwrap().as_deref(), Some("Four panes"));
        let (id, reply) = commands
            .begin("/split".into(), Some(Some("DEV".into())))
            .unwrap();
        commands.abandon(&id);
        assert!(!commands.claim(&id));
        assert!(reply.await.is_err());
    }
}
