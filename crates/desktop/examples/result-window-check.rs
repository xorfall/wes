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

const PAGE: &str = r#"<!doctype html><title>native fixture</title>
<input id="draft" value="synthetic unsent draft">
<script>
const route = location.hash;
if (!route) {
  fetch('/ready');
} else {
  const tab = route.split('/').pop();
  document.title = 'synthetic ' + tab;
  const valid = window.__WES_DESKTOP__ === true &&
    window.opener?.document.getElementById('draft').value === 'synthetic unsent draft';
  setTimeout(() => fetch(valid ? '/loaded/' + tab : '/failed'), 100);
}
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
    for tab in ["result", "json", "details", "result", "json", "details"] {
        main.eval(&format!("window.open('#open/synthetic/{tab}', '_blank');"))
            .map_err(|e| e.to_string())?;
        next(&messages, &format!("/loaded/{tab}"))?;
        let mut results = app
            .webview_windows()
            .into_iter()
            .filter(|(label, _)| label != "main");
        let (label, result) = results.next().ok_or("result window missing")?;
        if identity(&result, &messages)? == owner {
            return Err("result window inherited terminal ownership".into());
        }
        if results.next().is_some() {
            return Err("unexpected extra result window".into());
        }
        let address = result.url().map_err(|e| e.to_string())?;
        if address.fragment() != Some(format!("open/synthetic/{tab}").as_str()) {
            return Err(format!("incorrect route: {address}"));
        }
        if result.title().map_err(|e| e.to_string())? != format!("synthetic {tab}") {
            return Err("native title did not follow the result".into());
        }
        result.eval("window.close()").map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.get_webview_window(&label).is_some() {
            if Instant::now() >= deadline {
                return Err("result window did not close".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if app.get_webview_window("main").is_none() {
            return Err("main window was closed".into());
        }
        main.eval("fetch(document.getElementById('draft').value === 'synthetic unsent draft' && !location.hash ? '/main-ok' : '/failed')")
            .map_err(|e| e.to_string())?;
        next(&messages, "/main-ok")?;
        println!("PASS {tab}: route, title, desktop script, close and main draft");
    }
    main.eval("location.reload()").map_err(|e| e.to_string())?;
    next(&messages, "/ready")?;
    if identity(&main, &messages)? != owner {
        return Err("child configuration changed the main window's reload identity".into());
    }
    println!("PASS reload after child windows: main ownership remains unchanged");
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
                    match windows::result_window(&opener, url.as_str(), features, true, &origin) {
                        Ok(window) => {
                            let _ = window.hide();
                            NewWindowResponse::Create { window }
                        }
                        Err(error) => {
                            eprintln!("FAIL create: {error}");
                            opener.exit(1);
                            NewWindowResponse::Deny
                        }
                    }
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
