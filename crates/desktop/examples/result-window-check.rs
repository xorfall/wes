//! Run explicitly on macOS: cargo run -p wes-desktop --release --example result-window-check
//! No engine, user workspace, provider or preferences; only an ephemeral loopback fixture.
#[path = "../src/windows.rs"]
#[allow(dead_code)]
mod windows;

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder, webview::NewWindowResponse};

const PAGE: &str = r#"<!doctype html><meta charset="utf-8"><title>native fixture</title>
<input id="draft" value="synthetic unsent draft">
<script>
function draw() {
  if (!location.hash) { fetch('/ready'); return; }
  const [path, query] = location.hash.slice(1).split('?');
  const tab = path.split('/').pop();
  const workspace = new URLSearchParams(query).get('workspace') || 'none';
  document.title = 'synthetic ' + tab + ' · ' + workspace;
  const valid = window.__WES_DESKTOP__ === true &&
    window.opener?.document.getElementById('draft').value === 'synthetic unsent draft';
  setTimeout(() => fetch(valid ? '/loaded/' + tab + '/' + workspace : '/failed'), 100);
}
window.__WES_RETARGET__ = address => { history.replaceState(null, '', address); draw(); };
draw();
</script>"#;

fn next(messages: &Receiver<String>, expected: &str) -> Result<(), String> {
    let received = messages
        .recv_timeout(Duration::from_secs(15))
        .map_err(|e| e.to_string())?;
    if received != expected {
        return Err(format!("expected {expected}, received {received}"));
    }
    Ok(())
}

fn identity(window: &tauri::WebviewWindow, messages: &Receiver<String>) -> Result<String, String> {
    window
        .eval("if(window.__WES_WINDOW_CLIENT__) fetch('/identity/' + window.__WES_WINDOW_CLIENT__); else window.addEventListener('wes:window-client', () => fetch('/identity/' + window.__WES_WINDOW_CLIENT__), {once:true})")
        .map_err(|e| e.to_string())?;
    let received = messages
        .recv_timeout(Duration::from_secs(15))
        .map_err(|e| e.to_string())?;
    let id = received
        .strip_prefix("/identity/")
        .ok_or("identity response missing")?;
    uuid::Uuid::parse_str(id).map_err(|e| e.to_string())?;
    Ok(id.to_owned())
}

fn check(app: &tauri::AppHandle, messages: Receiver<String>) -> Result<(), String> {
    next(&messages, "/ready")?;
    let main = app
        .get_webview_window("main")
        .ok_or("main window missing")?;
    let owner = identity(&main, &messages)?;
    main.eval("location.reload()").map_err(|e| e.to_string())?;
    next(&messages, "/ready")?;
    if identity(&main, &messages)? != owner {
        return Err("native window identity changed on reload".into());
    }
    println!("PASS reload: native window ownership survives document recreation");
    let mut previous = String::new();
    for round in 0..2 {
        let mut opened = None;
        for (tab, workspace) in [("result", "alpha"), ("json", "beta"), ("details", "alpha")] {
            main.eval(&format!(
                "window.open('#open/synthetic/{tab}?workspace={workspace}', '_blank');"
            ))
            .map_err(|e| e.to_string())?;
            next(&messages, &format!("/loaded/{tab}/{workspace}"))?;
            let mut results = app
                .webview_windows()
                .into_iter()
                .filter(|(label, _)| label != "main");
            let (label, result) = results.next().ok_or("result window missing")?;
            let client = identity(&result, &messages)?;
            if client == owner || client == previous {
                return Err("result window inherited old ownership".into());
            }
            if let Some((old_label, old_client)) = &opened {
                if old_label != &label || old_client != &client {
                    return Err("retarget replaced the native window or owner".into());
                }
            } else {
                opened = Some((label.clone(), client));
            }
            if results.next().is_some() {
                return Err("unexpected extra result window".into());
            }
            let address = result.url().map_err(|e| e.to_string())?;
            if address.fragment()
                != Some(format!("open/synthetic/{tab}?workspace={workspace}").as_str())
            {
                return Err(format!("incorrect route: {address}"));
            }
            let title = result.title().map_err(|e| e.to_string())?;
            if title != format!("synthetic {tab} · {workspace}") {
                return Err(format!("native title did not follow the result: {title:?}"));
            }
            println!(
                "PASS retarget {round}/{tab}: one window, same ownership, requested workspace and title"
            );
        }
        let (label, client) = opened.unwrap();
        close(app, &label)?;
        previous = client;
        main.eval("fetch(document.getElementById('draft').value === 'synthetic unsent draft' && !location.hash ? '/main-ok' : '/failed')")
            .map_err(|e| e.to_string())?;
        next(&messages, "/main-ok")?;
    }
    main.eval("window.open('#graph?workspace=alpha', '_blank')")
        .map_err(|e| e.to_string())?;
    next(&messages, "/loaded/graph/alpha")?;
    let graph = app
        .webview_windows()
        .into_iter()
        .find(|(label, _)| label != "main")
        .ok_or("graph missing")?;
    let graph_owner = identity(&graph.1, &messages)?;
    main.eval("window.open('#stale?workspace=beta', '_blank')")
        .map_err(|e| e.to_string())?;
    next(&messages, "/loaded/stale/beta")?;
    if app.webview_windows().len() != 2 || identity(&graph.1, &messages)? != graph_owner {
        return Err("graph reuse changed its ownership".into());
    }
    main.eval("window.open('#open/synthetic/result?workspace=beta', '_blank')")
        .map_err(|e| e.to_string())?;
    next(&messages, "/loaded/result/beta")?;
    if app.webview_windows().len() != 3 {
        return Err("different purposes do not have their own windows".into());
    }
    let result = app
        .webview_windows()
        .into_iter()
        .find(|(label, _)| label != "main" && label != &graph.0)
        .unwrap();
    if identity(&result.1, &messages)? == graph_owner {
        return Err("different purposes share ownership".into());
    }
    close(app, &result.0)?;
    close(app, &graph.0)?;
    println!("PASS graph workspace switch: purpose reuse, separate result owner and close/reopen");
    main.eval("location.reload()").map_err(|e| e.to_string())?;
    next(&messages, "/ready")?;
    if identity(&main, &messages)? != owner {
        return Err("child configuration changed the main window's reload identity".into());
    }
    println!("PASS reload after child windows: main ownership remains unchanged");
    Ok(())
}

fn close(app: &tauri::AppHandle, label: &str) -> Result<(), String> {
    app.get_webview_window(label)
        .ok_or("window missing")?
        .eval("window.close()")
        .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.get_webview_window(label).is_some() {
        if Instant::now() >= deadline {
            return Err("detached window did not close".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if app.get_webview_window("main").is_none() {
        return Err("main window was closed".into());
    }
    Ok(())
}

fn main() {
    let failed = Arc::new(AtomicBool::new(true));
    let result = failed.clone();
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let address = listener.local_addr().unwrap();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut request = [0; 4096];
            let Ok(count) = stream.read(&mut request) else {
                continue;
            };
            let request = String::from_utf8_lossy(&request[..count]);
            let path = request.split_whitespace().nth(1).unwrap_or("/");
            if path == "/ready"
                || path.starts_with("/loaded/")
                || path == "/main-ok"
                || path == "/failed"
                || path.starts_with("/identity/")
            {
                let _ = send.send(path.to_string());
            }
            let body = if path == "/" { PAGE } else { "ok" };
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    let app = tauri::Builder::default()
        .setup(move |app| {
            let opener = app.handle().clone();
            let client: tauri::Url = format!("http://{address}/").parse()?;
            let origin: windows::Origin =
                std::sync::Arc::new(std::sync::RwLock::new(client.clone()));
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(client))
                .incognito(true)
                .visible(false)
                .initialization_script(windows::SHELL)
                .on_page_load(windows::window_client())
                .on_new_window(move |url, features| {
                    let response =
                        windows::requested_window(&opener, &url, features, true, &origin);
                    if let NewWindowResponse::Create { window } = &response {
                        let _ = window.hide();
                    }
                    response
                })
                .build()?;
            let handle = app.handle().clone();
            std::thread::spawn(move || match check(&handle, receive) {
                Ok(()) => {
                    result.store(false, Ordering::Relaxed);
                    handle.exit(0);
                }
                Err(error) => {
                    eprintln!("FAIL {error}");
                    handle.exit(1);
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("native fixture app");
    let _ = app.run_return(|_, _| {});
    if failed.load(Ordering::Relaxed) {
        std::process::exit(1);
    }
}
