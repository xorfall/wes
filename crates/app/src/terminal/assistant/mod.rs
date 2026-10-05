//! Assistant application boundary; the engine knows neither MCP nor model vendors.
mod editor;
mod inspection;
mod launch;
mod metrics;
mod protocol;
mod spec;
mod tools;
mod view_authoring;
pub(super) use editor::Editor;
pub use editor::EditorRequest;
pub(super) use launch::prepare;
pub(super) use tools::{State, dispatch};
pub const GUIDE: &str = include_str!("agent-instructions.txt");

pub async fn entry() -> Option<u8> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--assistant-mcp") => {
            let runtime = tokio::runtime::Handle::current();
            Some(
                tokio::task::spawn_blocking(move || protocol::serve(runtime))
                    .await
                    .unwrap_or(1),
            )
        }
        Some("--assistant-launch") => Some(launch::run(args.collect())),
        _ => None,
    }
}
