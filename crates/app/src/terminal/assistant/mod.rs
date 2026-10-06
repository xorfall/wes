//! Assistant application boundary; the engine knows neither MCP nor model vendors.
mod inspection;
mod launch;
mod metrics;
mod protocol;
mod spec;
mod tools;
mod view_authoring;
pub(super) use launch::prepare;
pub(super) use tools::{State, dispatch};
pub const GUIDE: &str = include_str!("agent-instructions.txt");

pub async fn entry() -> Option<u8> {
    let mut args = std::env::args().skip(1);
    // Every argument of a published pane program belongs to that program. A client's name in
    // the control directory is its launcher; any other name is a workspace command.
    if let Some((name, control)) = super::bridge::published_tool() {
        if !control || !launch::client(&name) {
            return None;
        }
        // The client owns the interrupt while it runs; this launcher must outlive it.
        #[cfg(windows)]
        let _interrupt = tokio::signal::windows::ctrl_c();
        return Some(launch::run(std::iter::once(name).chain(args).collect()));
    }
    match args.next().as_deref() {
        Some("--assistant-mcp") => {
            let connection = match (args.next().as_deref(), args.next()) {
                (None, _) => super::bridge::Connection::from_env(),
                (Some("--bridge"), Some(file)) if args.next().is_none() => {
                    super::bridge::Connection::from_file(std::path::Path::new(&file))
                }
                _ => {
                    eprintln!("Use --assistant-mcp [--bridge FILE].");
                    return Some(2);
                }
            };
            let runtime = tokio::runtime::Handle::current();
            Some(
                tokio::task::spawn_blocking(move || protocol::serve(runtime, connection))
                    .await
                    .unwrap_or(1),
            )
        }
        Some("--assistant-launch") => Some(launch::run(args.collect())),
        _ => None,
    }
}
