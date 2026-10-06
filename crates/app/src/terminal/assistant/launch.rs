use super::GUIDE;
use serde_json::{Value, json};
use std::{
    ffi::OsStr,
    io,
    path::{Path, PathBuf},
};

const CLIENTS: &[&str] = &["claude", "opencode", "codex"];
pub(super) fn client(name: &str) -> bool {
    CLIENTS.contains(&name)
}

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
    use crate::terminal::shell_quote;
    let agents = directory.join("assistants");
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
#[cfg(windows)]
fn write(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?
        .write_all(contents)
}
/// A client's launcher is this executable under the client's name. The workspace server is
/// the real executable started directly: a client that passes on none of the pane's
/// environment still reaches the bridge through the pane's own record of it.
#[cfg(windows)]
pub(in crate::terminal) fn prepare(
    directory: &Path,
    executable: &Path,
    url: &str,
    token: &str,
) -> io::Result<()> {
    for name in CLIENTS {
        crate::terminal::published::control(directory, name)?;
    }
    write(&directory.join("assistant-guide.txt"), GUIDE.as_bytes())?;
    let bridge = directory.join("bridge.json");
    write(
        &bridge,
        json!({"url": url, "token": token}).to_string().as_bytes(),
    )?;
    let text = |path: &Path| {
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| io::Error::other("The workspace server requires UTF-8 paths."))
    };
    write(
        &directory.join("mcp.json"),
        json!({"mcpServers":{"wes_workspace":{"type":"stdio","command":text(executable)?,
            "args":["--assistant-mcp","--bridge",text(&bridge)?]}}})
        .to_string()
        .as_bytes(),
    )
}
#[cfg(not(any(unix, windows)))]
pub(in crate::terminal) fn prepare(_: &Path, _: &Path, _: &str, _: &str) -> io::Result<()> {
    Err(io::Error::other("Terminal requires Unix."))
}

/// The command a client starts for this pane's workspace server.
#[cfg(unix)]
fn server(directory: &Path) -> Result<(PathBuf, Vec<String>), String> {
    Ok((directory.join("wes-mcp"), vec![]))
}
#[cfg(not(unix))]
fn server(directory: &Path) -> Result<(PathBuf, Vec<String>), String> {
    let missing = || "This Shell has no workspace server configuration.".to_owned();
    let text = std::fs::read_to_string(directory.join("mcp.json")).map_err(|_| missing())?;
    let config: Value = serde_json::from_str(&text).map_err(|_| missing())?;
    let server = &config["mcpServers"]["wes_workspace"];
    let command = server["command"].as_str().ok_or_else(missing)?;
    let arguments = server["args"]
        .as_array()
        .ok_or_else(missing)?
        .iter()
        .map(|argument| argument.as_str().map(str::to_owned).ok_or_else(missing))
        .collect::<Result<_, _>>()?;
    Ok((command.into(), arguments))
}

#[cfg(unix)]
const PROGRAM_EXTENSIONS: &[&str] = &[""];
/// A native program before a script launcher of the same name.
#[cfg(not(unix))]
const PROGRAM_EXTENSIONS: &[&str] = &[".exe", ".cmd", ".bat"];

fn real_executable(name: &str, wrapper: &Path, search: Option<&OsStr>) -> Option<PathBuf> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(search?)
        .filter(|path| path != wrapper)
        .find_map(|path| {
            PROGRAM_EXTENSIONS.iter().find_map(|extension| {
                let file = format!("{name}{extension}");
                let candidate = path.join(&file);
                let meta = candidate.metadata().ok()?;
                if !meta.is_file() {
                    return None;
                }
                #[cfg(unix)]
                if meta.permissions().mode() & 0o111 == 0 {
                    return None;
                }
                // A sourced shell config can add an equivalent path; do not recurse into our
                // wrapper.
                if candidate.canonicalize().ok() == wrapper.join(&file).canonicalize().ok() {
                    return None;
                }
                Some(candidate)
            })
        })
}
fn opencode_config(
    existing: Option<&str>,
    mcp: &Path,
    arguments: &[String],
) -> Result<String, &'static str> {
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
    let mut command = vec![json!(mcp)];
    command.extend(arguments.iter().map(|argument| json!(argument)));
    servers.insert(
        "wes_workspace".into(),
        json!({"type":"local","command":command,"enabled":true}),
    );
    Ok(config.to_string())
}

fn codex_config(mcp: &Path, arguments: &[String]) -> Result<[String; 3], &'static str> {
    let path = mcp
        .to_str()
        .ok_or("Codex MCP command requires a UTF-8 path.")?;
    // JSON's emitted string escapes also form a TOML basic string. Pass it as one
    // literal argv item, without shell interpretation. Preserve the server's policy
    // fields by changing only the transport fields owned by this attachment.
    let quoted = serde_json::to_string(path).expect("string serialization");
    Ok([
        format!("mcp_servers.wes_workspace.command={quoted}"),
        format!(
            "mcp_servers.wes_workspace.args={}",
            serde_json::to_string(arguments).expect("string serialization")
        ),
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
    let real = real_executable(name, &directory.join("assistants"), std::env::var_os("PATH").as_deref()).ok_or_else(|| format!("{name} is not installed on this Shell's PATH. Install/configure it separately; no download was attempted."))?;
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
                let (server, arguments) = server(&directory)?;
                command.env(
                    "OPENCODE_CONFIG_CONTENT",
                    opencode_config(existing.as_deref(), &server, &arguments)?,
                );
            }
            "codex" => {
                let (server, arguments) = server(&directory)?;
                for option in codex_config(&server, &arguments)? {
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
        let options = codex_config(path, &[]).unwrap();
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
        let value: Value = serde_json::from_str(&opencode_config(Some(r#"{"model":"local/model","permission":"ask","mcp":{"other":{"type":"remote","url":"https://invalid"}}}"#), Path::new("/fixture/wes-mcp"), &[]).unwrap()).unwrap();
        assert_eq!(value["model"], "local/model");
        assert_eq!(value["permission"], "ask");
        assert_eq!(value["mcp"]["other"]["url"], "https://invalid");
        assert_eq!(
            value["mcp"]["wes_workspace"]["command"][0],
            "/fixture/wes-mcp"
        );
        assert!(
            opencode_config(
                Some(r#"{"mcp":{"wes_workspace":{}}}"#),
                Path::new("/x"),
                &[]
            )
            .is_err()
        );
        assert!(opencode_config(Some("[]"), Path::new("/x"), &[]).is_err());
    }
    #[test]
    fn a_directly_started_server_keeps_its_arguments_in_every_clients_form() {
        let arguments = [
            "--assistant-mcp".to_owned(),
            r"C:\boş luk\bridge.json".to_owned(),
        ];
        let options = codex_config(Path::new(r"C:\wes\wes.exe"), &arguments).unwrap();
        let listed: Vec<String> =
            serde_json::from_str(options[1].split_once('=').unwrap().1).unwrap();
        assert_eq!(listed, arguments);
        let value: Value = serde_json::from_str(
            &opencode_config(None, Path::new(r"C:\wes\wes.exe"), &arguments).unwrap(),
        )
        .unwrap();
        assert_eq!(
            value["mcp"]["wes_workspace"]["command"],
            json!([
                r"C:\wes\wes.exe",
                "--assistant-mcp",
                r"C:\boş luk\bridge.json"
            ])
        );
    }
    #[cfg(windows)]
    #[test]
    fn a_client_is_a_program_before_a_script_and_never_its_own_launcher() {
        let root = tempfile::tempdir().unwrap();
        let (launchers, scripts, programs) = (
            root.path().join("assistants"),
            root.path().join("scripts"),
            root.path().join("boş luk"),
        );
        for directory in [&launchers, &scripts, &programs] {
            std::fs::create_dir(directory).unwrap();
        }
        std::fs::write(launchers.join("claude.exe"), "launcher").unwrap();
        std::fs::write(scripts.join("claude.cmd"), "script").unwrap();
        std::fs::write(programs.join("claude.cmd"), "script").unwrap();
        std::fs::write(programs.join("claude.exe"), "program").unwrap();
        let search = |order: &[&PathBuf]| std::env::join_paths(order.iter().copied()).unwrap();
        // The launcher's own directory is skipped however it is spelled on the search path.
        let respelled = PathBuf::from(launchers.to_str().unwrap().to_uppercase());
        assert_eq!(
            real_executable(
                "claude",
                &launchers,
                Some(&search(&[&launchers, &respelled, &scripts, &programs]))
            ),
            Some(scripts.join("claude.cmd"))
        );
        assert_eq!(
            real_executable("claude", &launchers, Some(&search(&[&programs, &scripts]))),
            Some(programs.join("claude.exe"))
        );
        assert_eq!(
            real_executable("codex", &launchers, Some(&search(&[&programs]))),
            None
        );
        assert_eq!(real_executable("claude", &launchers, None), None);
    }
    #[cfg(windows)]
    #[test]
    fn the_server_a_client_starts_is_read_from_the_panes_own_configuration() {
        let directory = tempfile::tempdir().unwrap();
        assert!(server(directory.path()).is_err());
        std::fs::write(
            directory.path().join("mcp.json"),
            r#"{"mcpServers":{"wes_workspace":{"type":"stdio","command":"C:\\wes\\wes.exe","args":["--assistant-mcp","--bridge","C:\\p\\bridge.json"]}}}"#,
        )
        .unwrap();
        assert_eq!(
            server(directory.path()).unwrap(),
            (
                PathBuf::from(r"C:\wes\wes.exe"),
                vec![
                    "--assistant-mcp".to_owned(),
                    "--bridge".to_owned(),
                    r"C:\p\bridge.json".to_owned()
                ]
            )
        );
    }
}
