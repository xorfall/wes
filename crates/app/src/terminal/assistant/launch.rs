use super::GUIDE;
use crate::terminal::shell_quote;
use serde_json::{Value, json};
use std::{
    io,
    path::{Path, PathBuf},
};

const CLIENTS: &[&str] = &["claude", "opencode", "codex"];

#[cfg(unix)]
fn write(path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)?;
    file.write_all(contents)
}
#[cfg(unix)]
pub(in crate::terminal) fn prepare(
    directory: &Path,
    executable: &Path,
    url: &str,
    token: &str,
) -> io::Result<()> {
    let agents = directory.join("assistants");
    std::fs::create_dir(&agents)?;
    let executable = shell_quote(&executable.to_string_lossy());
    write(&directory.join("wes-mcp"), format!("#!/bin/sh\nexport WES_BRIDGE_URL={}\nexport WES_BRIDGE_TOKEN={}\nexec {executable} --assistant-mcp \"$@\"\n", shell_quote(url), shell_quote(token)).as_bytes(), 0o700)?;
    for name in CLIENTS {
        write(
            &agents.join(name),
            format!("#!/bin/sh\nexec {executable} --assistant-launch {name} \"$@\"\n").as_bytes(),
            0o700,
        )?;
    }
    write(
        &directory.join("assistant-guide.txt"),
        GUIDE.as_bytes(),
        0o600,
    )?;
    let mcp = directory.join("wes-mcp");
    write(
        &directory.join("mcp.json"),
        json!({"mcpServers":{"wes_workspace":{"type":"stdio","command":mcp,"args":[]}}})
            .to_string()
            .as_bytes(),
        0o600,
    )
}
#[cfg(not(unix))]
pub(in crate::terminal) fn prepare(_: &Path, _: &Path, _: &str, _: &str) -> io::Result<()> {
    Err(io::Error::other("Terminal requires Unix."))
}

fn real_executable(name: &str, wrapper: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|path| path != wrapper)
        .find_map(|path| {
            let candidate = path.join(name);
            let meta = candidate.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            #[cfg(unix)]
            if meta.permissions().mode() & 0o111 == 0 {
                return None;
            }
            // A sourced shell config can add an equivalent path; do not recurse into our wrapper.
            if candidate.canonicalize().ok() == wrapper.join(name).canonicalize().ok() {
                return None;
            }
            Some(candidate)
        })
}
fn opencode_config(existing: Option<&str>, mcp: &Path) -> Result<String, &'static str> {
    let mut config: Value = match existing {
        Some(text) => serde_json::from_str(text).map_err(
            |_| "OPENCODE_CONFIG_CONTENT must be a JSON object for automatic attachment.",
        )?,
        None => json!({}),
    };
    let object = config
        .as_object_mut()
        .ok_or("OpenCode inline settings must be an object.")?;
    let servers = object
        .entry("mcp")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("OpenCode mcp settings must be an object.")?;
    if servers.contains_key("wes_workspace") {
        return Err(
            "MCP name wes_workspace already exists; use WES_AGENT_BYPASS=1 to launch without automatic attachment.",
        );
    }
    servers.insert(
        "wes_workspace".into(),
        json!({"type":"local","command":[mcp],"enabled":true}),
    );
    Ok(config.to_string())
}

fn codex_config(mcp: &Path) -> Result<[String; 3], &'static str> {
    let path = mcp
        .to_str()
        .ok_or("Codex MCP command requires a UTF-8 path.")?;
    // JSON's emitted string escapes also form a TOML basic string. Pass it as one
    // literal argv item, without shell interpretation. Preserve the server's policy
    // fields by changing only the transport fields owned by this attachment.
    let quoted = serde_json::to_string(path).expect("string serialization");
    Ok([
        format!("mcp_servers.wes_workspace.command={quoted}"),
        "mcp_servers.wes_workspace.args=[]".into(),
        "mcp_servers.wes_workspace.env_vars=[\"WES_MCP_METRICS_DIR\"]".into(),
    ])
}
pub(super) fn run(args: Vec<String>) -> u8 {
    match start(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("wes assistant: {message}");
            2
        }
    }
}
fn start(args: Vec<String>) -> Result<u8, String> {
    let name = args
        .first()
        .filter(|s| CLIENTS.contains(&s.as_str()))
        .ok_or("Unknown assistant launcher.")?;
    let directory = PathBuf::from(
        std::env::var_os("WES_ASSISTANT_DIRECTORY")
            .ok_or("Start this assistant from a new wes Shell terminal.")?,
    );
    let real = real_executable(name, &directory.join("assistants")).ok_or_else(|| format!("{name} is not installed on this Shell's PATH. Install/configure it separately; no download was attempted."))?;
    let mut command = std::process::Command::new(real);
    let bypass = std::env::var("WES_AGENT_BYPASS").as_deref() == Ok("1");
    if !bypass {
        match name.as_str() {
            "claude" => {
                if args[1..]
                    .iter()
                    .any(|a| a == "--mcp-config" || a.starts_with("--mcp-config="))
                {
                    return Err("An explicit --mcp-config was supplied. Include the server from $WES_MCP_CONFIG in your configuration and launch with WES_AGENT_BYPASS=1; existing arguments were not changed.".into());
                }
                command.arg("--mcp-config").arg(directory.join("mcp.json"));
            }
            "opencode" => {
                let existing = std::env::var("OPENCODE_CONFIG_CONTENT").ok();
                command.env(
                    "OPENCODE_CONFIG_CONTENT",
                    opencode_config(existing.as_deref(), &directory.join("wes-mcp"))?,
                );
            }
            "codex" => {
                for option in codex_config(&directory.join("wes-mcp"))? {
                    command.arg("-c").arg(option);
                }
            }
            _ => unreachable!(),
        }
    }
    command.args(&args[1..]);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(format!("Could not start {name}: {}", command.exec()))
    }
    #[cfg(not(unix))]
    {
        Ok(command
            .status()
            .map_err(|_| "Could not start assistant.")?
            .code()
            .unwrap_or(1) as u8)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codex_attachment_owns_transport_only_and_quotes_the_literal_path() {
        let path = Path::new("/fixture/özel \"name\"/back\\slash/$(literal)/wes-mcp");
        let options = codex_config(path).unwrap();
        assert_eq!(
            serde_json::from_str::<String>(options[0].split_once('=').unwrap().1).unwrap(),
            path.to_str().unwrap()
        );
        assert_eq!(options[1], "mcp_servers.wes_workspace.args=[]");
        assert_eq!(
            options[2],
            "mcp_servers.wes_workspace.env_vars=[\"WES_MCP_METRICS_DIR\"]"
        );
        assert!(CLIENTS.contains(&"codex"));
    }
    #[test]
    fn inline_settings_preserve_other_models_servers_and_permissions() {
        let value: Value = serde_json::from_str(&opencode_config(Some(r#"{"model":"local/model","permission":"ask","mcp":{"other":{"type":"remote","url":"https://invalid"}}}"#), Path::new("/fixture/wes-mcp")).unwrap()).unwrap();
        assert_eq!(value["model"], "local/model");
        assert_eq!(value["permission"], "ask");
        assert_eq!(value["mcp"]["other"]["url"], "https://invalid");
        assert_eq!(
            value["mcp"]["wes_workspace"]["command"][0],
            "/fixture/wes-mcp"
        );
        assert!(opencode_config(Some(r#"{"mcp":{"wes_workspace":{}}}"#), Path::new("/x")).is_err());
        assert!(opencode_config(Some("[]"), Path::new("/x")).is_err());
    }
}
