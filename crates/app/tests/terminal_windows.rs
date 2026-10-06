//! Workspace commands in a Windows pane, through real processes: the published programs, the
//! arguments they receive, and a served engine with a PowerShell pane.
#![cfg(windows)]
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const WES: &str = env!("CARGO_BIN_EXE_wes");

/// A loopback bridge that records each request and answers with a fixed reply.
fn recording_bridge() -> (u16, Arc<Mutex<Vec<serde_json::Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(vec![]));
    let recorded = requests.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut head = vec![];
            while !head.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                head.push(byte[0]);
            }
            let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
            assert!(head.contains("authorization: bearer synthetic"), "{head}");
            let length: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            recorded
                .lock()
                .unwrap()
                .push(serde_json::from_slice(&body).unwrap());
            let reply = r#"{"code":7,"stdout":"RECORDED","stderr":""}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
    });
    (port, requests)
}
fn publish(program: &Path) {
    if std::fs::hard_link(WES, program).is_err() {
        std::fs::copy(WES, program).unwrap();
    }
}
fn pane(root: &Path) -> PathBuf {
    let directory = root.join("boş luk").join("pane");
    std::fs::create_dir_all(directory.join("assistants")).unwrap();
    publish(&directory.join("echo-tool.exe"));
    publish(&directory.join("assistants").join("wesx.exe"));
    directory
}
fn bridged(mut command: Command, port: u16, directory: &Path) -> std::process::Output {
    command
        .env(
            "WES_BRIDGE_URL",
            format!("http://127.0.0.1:{port}/terminal-bridge"),
        )
        .env("WES_BRIDGE_TOKEN", "synthetic")
        .env("WES_ASSISTANT_DIRECTORY", directory)
        .output()
        .unwrap()
}

#[test]
fn a_published_program_receives_its_arguments_exactly_as_passed() {
    let root = tempfile::tempdir().unwrap();
    let directory = pane(root.path());
    let (port, requests) = recording_bridge();
    let last = || requests.lock().unwrap().last().unwrap().clone();

    // Everything a batch file or a shell in between would reinterpret.
    let arguments = [
        "plain",
        "two words",
        "",
        "tırnak \"içinde\" kalan",
        "a&b|c>d<e^f%PATH%!g!",
        r"C:\yol\sonu\",
        r#"{"k":"v w","n":[1,2]}"#,
        "ĞğİıŞşÇçÖöÜü",
        "--flag=va lue",
        "'tek'",
        "--assistant-mcp",
        "--terminal-bridge",
    ];
    let mut command = Command::new(directory.join("echo-tool.exe"));
    command.args(arguments);
    let output = bridged(command, port, &directory);
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "RECORDED");
    assert_eq!(
        last(),
        serde_json::json!({"tool": "echo-tool", "args": arguments})
    );

    // The name is the directory's spelling whatever case the caller typed.
    let output = bridged(
        Command::new(directory.join("ECHO-TOOL.EXE")),
        port,
        &directory,
    );
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(last(), serde_json::json!({"tool": "echo-tool", "args": []}));

    let mut command = Command::new(directory.join("assistants").join("wesx.exe"));
    command.args(["--cmd", "/rsplit xterm"]);
    bridged(command, port, &directory);
    assert_eq!(
        last(),
        serde_json::json!({"tool": "wesx", "args": ["--cmd", "/rsplit xterm"]})
    );

    // Windows PowerShell passes these forms through unchanged, from a directory with spaces.
    let script = format!(
        "& '{}' 'two words' 'ĞğİıŞş' 'a&b|c>d' 'C:\\yol sonu\\x' --flag=1",
        directory.join("echo-tool.exe").display()
    );
    let root_directory = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
    let mut command =
        Command::new(root_directory.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"));
    command.args(["-NoLogo", "-NoProfile", "-Command", &script]);
    // The shell reports only that its last command failed, not the program's own code.
    let output = bridged(command, port, &directory);
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(
        last(),
        serde_json::json!({"tool": "echo-tool", "args": ["two words", "ĞğİıŞş", "a&b|c>d", "C:\\yol sonu\\x", "--flag=1"]})
    );

    // Outside a pane's own directory the same file is the ordinary program again.
    let seen = requests.lock().unwrap().len();
    let mut command = Command::new(directory.join("echo-tool.exe"));
    command.arg("--help");
    let output = bridged(command, port, root.path());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Usage: wes"));
    assert_eq!(requests.lock().unwrap().len(), seen);
}

struct Served(std::process::Child);
impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn result(file: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(40);
    loop {
        if let Ok(text) = std::fs::read_to_string(file)
            && text.ends_with("|END")
        {
            return text;
        }
        assert!(Instant::now() < deadline, "no result in {}", file.display());
        std::thread::sleep(Duration::from_millis(50));
    }
}

const CLIENT: &str = "synthetic-windows-ui";
/// A served engine in a spaced, non-ASCII working directory.
struct Engine {
    _served: Option<Served>,
    desktop: Option<wes::data_home::host::DesktopHost>,
    url: String,
    generation: String,
    http: reqwest::Client,
    work: PathBuf,
    _root: tempfile::TempDir,
}
impl Engine {
    async fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("çalışma alanı");
        std::fs::create_dir(&work).unwrap();
        let mut child = Command::new(WES)
            .arg("--home")
            .arg(root.path().join("home"))
            .args(["--serve", "0"])
            .current_dir(&work)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut announced = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut announced)
            .unwrap();
        let served = Served(child);
        let url = announced
            .trim()
            .strip_prefix("Listening at ")
            .unwrap_or_else(|| panic!("{announced:?}"))
            .to_owned();
        Self::connect(root, work, url, Some(served), None).await
    }
    async fn desktop() -> Self {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("desktop home");
        std::fs::create_dir(&work).unwrap();
        let host = wes::data_home::host::DesktopHost::start_with_terminal(
            work.clone(),
            None,
            PathBuf::from(WES),
        )
        .await
        .unwrap();
        let url = host.location().url.trim_end_matches('/').to_owned();
        Self::connect(root, work, url, None, Some(host)).await
    }
    async fn connect(
        root: tempfile::TempDir,
        work: PathBuf,
        url: String,
        served: Option<Served>,
        desktop: Option<wes::data_home::host::DesktopHost>,
    ) -> Self {
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut events = http.get(format!("{url}/events")).send().await.unwrap();
        let mut stream = String::new();
        let generation = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let chunk = events.chunk().await.unwrap().expect("event stream");
                stream.push_str(&String::from_utf8_lossy(&chunk));
                let found = stream
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:"))
                    .filter_map(|data| serde_json::from_str::<serde_json::Value>(data).ok())
                    .find(|event| event["event"] == "session");
                if let Some(event) = found {
                    break event["generation"].as_str().unwrap().to_owned();
                }
            }
        })
        .await
        .unwrap();
        Self {
            _served: served,
            desktop,
            url,
            generation,
            http,
            work,
            _root: root,
        }
    }
    async fn post(&self, path: &str, body: serde_json::Value) -> serde_json::Value {
        let response = self
            .http
            .post(format!("{}{path}", self.url))
            .header("X-Wes-Session", &self.generation)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .unwrap();
        let status = response.status();
        let text = response.text().await.unwrap();
        assert!(status.is_success(), "{path}: {status} {text}");
        serde_json::from_str(&text).unwrap()
    }
    async fn pane(&self) -> String {
        self.post(
            "/terminals",
            serde_json::json!({"client": CLIENT, "action": "start"}),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    /// One line in the pane. Its output, its exit code and a marker are left in a file.
    async fn run(&self, pane: &str, name: &str, command: &str) -> String {
        let file = self.work.join(format!("{name}.txt"));
        let _ = std::fs::remove_file(&file);
        let text = format!(
            "$o = {command}; [IO.File]::WriteAllText(\"$PWD\\{name}.txt\", ($o -join \"`n\") + '|' + $LASTEXITCODE + '|END')\r"
        );
        self.post(
            "/terminals",
            serde_json::json!({"client": CLIENT, "action": "write", "id": pane, "text": text}),
        )
        .await;
        result(&file)
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(host) = self.desktop.take() {
            // Join the desktop's owners before its isolated home is removed, even on failure.
            let _ = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(host.shutdown())
            });
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_served_pane_runs_the_workspace_commands_from_powershell() {
    let engine = Engine::start().await;
    engine
        .post(
            "/submit",
            serde_json::json!({"request": "submit", "cell": uuid::Uuid::new_v4().to_string(),
                "text": ":calc { return \"çalışma alanı\"; } > metin", "client": CLIENT,
                "environments": null, "console": false}),
        )
        .await;
    let pane = engine.pane().await;

    let help = engine.run(&pane, "help", "wesx --help").await;
    assert!(
        help.contains("wesx provider") && help.ends_with("|0|END"),
        "{help}"
    );
    let list = engine.run(&pane, "list", "wesx provider --list").await;
    assert!(list.contains("http") && list.ends_with("|0|END"), "{list}");

    // The value was computed by the engine; the pane reads it with its non-ASCII text intact.
    let deadline = Instant::now() + Duration::from_secs(30);
    let value = loop {
        let value = engine.run(&pane, "value", "wesx value get metin").await;
        if value.ends_with("|0|END") {
            break value;
        }
        assert!(Instant::now() < deadline, "{value}");
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    assert!(value.contains("çalışma alanı"), "{value}");
    let missing = engine.run(&pane, "missing", "wesx value get yok").await;
    assert!(!missing.ends_with("|0|END"), "{missing}");

    // Provider names are programs after the host's own; the control directory comes first and
    // holds the assistant launchers.
    let found = engine
        .run(
            &pane,
            "where",
            "(Get-Command http).Source, (Get-Command wesx).Source, (Get-Command wes-value).Source, (Get-Command claude).Source",
        )
        .await
        .to_ascii_lowercase();
    let programs: Vec<_> = found.lines().collect();
    assert!(programs[0].ends_with(r"\http.exe"), "{found}");
    assert!(programs[1].ends_with(r"\assistants\wesx.exe"), "{found}");
    assert!(programs[2].contains(r"\wes-value.exe"), "{found}");
    assert!(programs[3].contains(r"\assistants\claude.exe"), "{found}");
    assert!(
        std::fs::read_dir(Path::new(programs[0]).parent().unwrap())
            .unwrap()
            .all(|entry| {
                let name = entry.unwrap().file_name().into_string().unwrap();
                !name.ends_with(".cmd") && !name.ends_with(".bat")
            })
    );

    engine
        .post(
            "/terminals",
            serde_json::json!({"client": CLIENT, "action": "close", "id": pane}),
        )
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn desktop_client_launchers_share_the_panes_console_and_wait_for_input() {
    let engine = Engine::desktop().await;
    let fixture = engine.work.join("clients");
    std::fs::create_dir(&fixture).unwrap();
    let source = fixture.join("client.cs");
    std::fs::write(&source, include_str!("fixtures/terminal_client.cs")).unwrap();
    let executable = fixture.join("claude.exe");
    let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let compiled = Command::new(powershell)
        .args(["-NoLogo", "-NoProfile", "-Command"])
        .arg(format!(
            "Add-Type -Path '{}' -OutputAssembly '{}' -OutputType ConsoleApplication",
            source.display(),
            executable.display()
        ))
        .output()
        .unwrap();
    assert!(compiled.status.success(), "{compiled:?}");
    for client in ["codex", "opencode"] {
        std::fs::copy(&executable, fixture.join(format!("{client}.exe"))).unwrap();
    }
    let pane = engine.pane().await;
    let located = engine
        .run(&pane, "client-directory", "$env:WES_ASSISTANT_DIRECTORY")
        .await;
    let control = PathBuf::from(located.split('|').next().unwrap()).join("assistants");
    // The real system clients never appear on this pane's search path.
    engine
        .run(
            &pane,
            "client-path",
            &format!(
                "& {{ $env:PATH = '{};{};{}\\System32'; $PID }}",
                control.display(),
                fixture.display(),
                std::env::var("SystemRoot").unwrap()
            ),
        )
        .await;
    let shell = result(&engine.work.join("client-path.txt"))
        .split('|')
        .next()
        .unwrap()
        .parse::<u32>()
        .unwrap();
    for name in ["claude", "codex", "opencode"] {
        let launcher = control.join(format!("{name}.exe"));
        let image = std::fs::read(&launcher).unwrap();
        let pe = u32::from_le_bytes(image[60..64].try_into().unwrap()) as usize;
        assert_eq!(
            &image[pe + 24 + 68..pe + 24 + 70],
            &[3, 0],
            "the desktop published a GUI process"
        );
        let report = engine.work.join(format!("{name}-console.txt"));
        let done = engine.work.join(format!("{name}-done.txt"));
        engine.post("/terminals", serde_json::json!({"client": CLIENT, "action": "write", "id": pane,
            "text": format!("$env:WES_TEST_CONSOLE = '{}'; {name}; [IO.File]::WriteAllText('{}', $LASTEXITCODE.ToString() + '|END')\r", report.display(), done.display())})).await;
        let ready = result(&report);
        let info: serde_json::Value = serde_json::from_str(ready.trim_end_matches("|END")).unwrap();
        assert!(
            info["console"]
                .as_array()
                .unwrap()
                .iter()
                .any(|pid| pid.as_u64() == Some(shell as u64)),
            "{name} left the pane's console: {info}"
        );
        assert_eq!(info["inputRedirected"], false, "{info}");
        assert_eq!(info["outputRedirected"], false, "{info}");
        assert!(!done.exists(), "the shell did not wait for {name}");
        // These are the client's automatic workspace connection arguments/settings.
        assert!(
            info["args"].as_str().unwrap().contains(match name {
                "claude" => "--mcp-config",
                "codex" => "mcp_servers.wes_workspace",
                _ => "",
            }),
            "{info}"
        );
        if name == "opencode" {
            assert!(
                info["config"].as_str().unwrap().contains("wes_workspace"),
                "{info}"
            );
        }
        engine
            .post(
                "/terminals",
                serde_json::json!({"client": CLIENT, "action": "write", "id": pane,
            "text": "pane-input\r"}),
            )
            .await;
        assert_eq!(result(&report.with_extension("input")), "pane-input|END");
        assert_eq!(result(&done), "23|END");
    }
    let report = engine.work.join("client-interrupt.txt");
    engine.post("/terminals", serde_json::json!({"client": CLIENT, "action": "write", "id": pane,
        "text": format!("$env:WES_TEST_CONSOLE = '{}'; claude --wait-for-interrupt\r", report.display())})).await;
    let ready = result(&report);
    let interrupted: serde_json::Value =
        serde_json::from_str(ready.trim_end_matches("|END")).unwrap();
    engine.post("/terminals", serde_json::json!({"client": CLIENT, "action": "resize", "id": pane, "cols": 104, "rows": 32})).await;
    engine
        .post(
            "/terminals",
            serde_json::json!({"client": CLIENT, "action": "write", "id": pane, "text": "\u{3}"}),
        )
        .await;
    assert_eq!(result(&report.with_extension("input")), "<interrupt>|END");
    // Ctrl+C cancels the whole PowerShell statement. A fresh command must run after
    // the client accepts it; statements after the interrupted command are not resumed.
    let after = engine.run(&pane, "after-interrupt", "wesx --help").await;
    assert!(
        after.contains("wesx provider") && after.ends_with("|0|END"),
        "{after}"
    );
    let stopped = engine.run(&pane, "client-stopped", &format!("if (Get-Process -Id {} -ErrorAction SilentlyContinue) {{ 'alive' }} else {{ 'stopped' }}", interrupted["pid"])).await;
    assert!(stopped.starts_with("stopped|"), "{stopped}");
    engine
        .post(
            "/terminals",
            serde_json::json!({"client": CLIENT, "action": "close", "id": pane}),
        )
        .await;
}

/// The server as a client starts it: the pane's configured command, none of the pane's
/// environment, and nothing but protocol messages on its output.
struct Server {
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
    lines: std::sync::mpsc::Receiver<String>,
}
impl Server {
    fn start(configuration: &Path) -> Self {
        let config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(configuration).unwrap()).unwrap();
        let server = &config["mcpServers"]["wes_workspace"];
        assert_eq!(server["type"], "stdio");
        let command = PathBuf::from(server["command"].as_str().unwrap());
        // The real executable, started directly: no script and no launcher in between.
        assert_eq!(
            command.canonicalize().unwrap(),
            Path::new(WES).canonicalize().unwrap()
        );
        let arguments: Vec<_> = server["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|argument| argument.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(arguments[..2], ["--assistant-mcp", "--bridge"]);
        let mut child = Command::new(command)
            .args(&arguments)
            .env_clear()
            .env("SystemRoot", std::env::var_os("SystemRoot").unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (sender, lines) = std::sync::mpsc::channel();
        let output = BufReader::new(child.stdout.take().unwrap());
        std::thread::spawn(move || {
            for line in output.lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            input: child.stdin.take(),
            child,
            lines,
        }
    }
    fn send(&mut self, message: serde_json::Value) {
        writeln!(self.input.as_mut().unwrap(), "{message}").unwrap();
    }
    /// The reply to one request. Every line on the way must itself be a protocol message.
    fn reply(&mut self, id: u64) -> serde_json::Value {
        loop {
            let line = self
                .lines
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_else(|_| panic!("no reply to request {id}"));
            let message: serde_json::Value = serde_json::from_str(&line)
                .unwrap_or_else(|_| panic!("not a protocol message: {line:?}"));
            assert_eq!(message["jsonrpc"], "2.0", "{line}");
            if message["id"] == id {
                return message;
            }
        }
    }
    fn call(&mut self, id: u64, name: &str, arguments: serde_json::Value) -> serde_json::Value {
        self.send(
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}}),
        );
        self.reply(id)
    }
    /// Closing the input ends the server; whatever it wrote besides replies is returned.
    fn finish(mut self) -> (Option<i32>, String, Vec<String>) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "the server outlived its input");
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut errors = String::new();
        self.child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut errors)
            .unwrap();
        (status.code(), errors, self.lines.try_iter().collect())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_assistants_server_speaks_only_its_protocol_and_loses_access_with_the_pane() {
    let engine = Engine::desktop().await;
    let pane = engine.pane().await;
    let located = engine.run(&pane, "mcp", "$env:WES_MCP_CONFIG").await;
    let configuration = PathBuf::from(located.split('|').next().unwrap());
    let bridge: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(configuration.with_file_name("bridge.json")).unwrap(),
    )
    .unwrap();

    let mut server = tokio::task::block_in_place(|| Server::start(&configuration));
    let (listed, help) = tokio::task::block_in_place(|| {
        server.send(
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "clientInfo": {}, "capabilities": {}}}),
        );
        let initialized = server.reply(1);
        assert_eq!(
            initialized["result"]["serverInfo"]["name"], "wes-workspace",
            "{initialized}"
        );
        server.send(serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        server.send(serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let listed = server.reply(2);
        // A workspace call through the pane's authority.
        let help = server.call(
            3,
            "help",
            serde_json::json!({"command": "node", "depth": 1}),
        );
        (listed, help)
    });
    assert!(
        listed["result"]["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "help")),
        "{listed}"
    );
    assert_eq!(help["result"]["isError"], false, "{help}");

    // The pane's token is the server's only authority; closing the pane ends it.
    engine
        .post(
            "/terminals",
            serde_json::json!({"client": CLIENT, "action": "close", "id": pane}),
        )
        .await;
    let refused = tokio::task::block_in_place(|| {
        server.call(
            4,
            "help",
            serde_json::json!({"command": "node", "depth": 1}),
        )
    });
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(
        refused["result"]["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.contains("authority")),
        "{refused}"
    );
    // The same holds for a workspace command that kept the pane's token.
    let command = Command::new(WES)
        .args(["--terminal-bridge", "wesx", "--help"])
        .env("WES_BRIDGE_URL", bridge["url"].as_str().unwrap())
        .env("WES_BRIDGE_TOKEN", bridge["token"].as_str().unwrap())
        .output()
        .unwrap();
    assert_eq!(command.status.code(), Some(3), "{command:?}");

    let (code, errors, extra) = tokio::task::block_in_place(|| server.finish());
    assert_eq!(code, Some(0));
    assert_eq!(errors, "");
    assert!(extra.is_empty(), "{extra:?}");
}
