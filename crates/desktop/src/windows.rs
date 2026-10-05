//! Native window creation shared with the isolated macOS acceptance fixture.
use std::sync::{
    Arc, RwLock,
    atomic::{AtomicUsize, Ordering},
};
use tauri::{
    Manager, Runtime, WebviewUrl, WebviewWindowBuilder,
    webview::{NewWindowFeatures, NewWindowResponse},
};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// What every window of this app is told about itself before its first script runs.
///
/// The client asks for a result in a window of its own with `window.open` and closes it with
/// `window.close`. A browser tab does both by itself; a webview does the first only because
/// the shell's `on_new_window` handler answers it, and the second not at all — WKWebView has no `webViewDidClose`
/// here. So `close` is redirected onto a navigation the shell recognises and cancels, which keeps
/// the client free of any knowledge of which shell it is running in.
pub(crate) const SHELL: &str = "window.__WES_DESKTOP__ = true; window.close = function () { window.location.replace('/__wes/close'); };";

/// The path `window.close` turns into. Never fetched: the navigation to it is always cancelled.
const CLOSING: &str = "/__wes/close";

/// Whether a navigation is the shell's own `window.close`, rather than somewhere the page is going.
///
/// The path and nothing else: a query or a fragment on it is still the same request, and any other
/// path — including one that merely contains this one — is the page navigating and must be allowed.
fn is_closing(url: &tauri::Url) -> bool {
    url.path() == CLOSING
}

/// The address the running client is served from. It changes when another data folder is opened.
pub(crate) type Origin = Arc<RwLock<tauri::Url>>;

/// Where an address the page asks for belongs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Destination {
    /// The client's own page: a window of this app.
    App,
    /// Somebody else's web page: the person's browser, never a window that runs the shell script.
    Browser(tauri::Url),
    /// Nothing that may be opened from the client.
    Refused,
}

/// The origin decides: the client's own pages stay in the app, other web pages go to the browser.
/// Addresses carrying credentials are refused rather than handed on, and so is any other scheme.
pub(crate) fn destination(requested: &tauri::Url, client: &tauri::Url) -> Destination {
    if requested.origin() == client.origin() {
        return Destination::App;
    }
    let web = matches!(requested.scheme(), "http" | "https");
    if web
        && requested.host().is_some()
        && requested.username().is_empty()
        && requested.password().is_none()
    {
        Destination::Browser(requested.clone())
    } else {
        Destination::Refused
    }
}

/// Hands one web address to the platform's opener: no shell, the address as a single argument.
fn open_in_browser(url: &tauri::Url) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("rundll32");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    let mut child = wes::serialized_spawn(|| {
        command
            .arg(url.as_str())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    })?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn open_elsewhere(url: &tauri::Url) {
    if let Err(error) = open_in_browser(url) {
        eprintln!("wes-desktop: could not open the browser: {error}");
        tracing::warn!(target: "wes.telemetry", notice = "ui_error");
    }
}

/// A window the page asked for with `window.open`: a result window, the browser, or nothing.
pub(crate) fn requested_window<R: Runtime>(
    app: &tauri::AppHandle<R>,
    url: &tauri::Url,
    features: NewWindowFeatures,
    incognito: bool,
    origin: &Origin,
) -> NewWindowResponse<R> {
    let client = match origin.read() {
        Ok(client) => client.clone(),
        Err(_) => return NewWindowResponse::Deny,
    };
    match destination(url, &client) {
        Destination::App => match result_window(app, url.as_str(), features, incognito, origin) {
            Ok(window) => NewWindowResponse::Create { window },
            Err(error) => {
                eprintln!("wes-desktop: could not open a window: {error}");
                tracing::warn!(target: "wes.telemetry", notice = "ui_error");
                NewWindowResponse::Deny
            }
        },
        Destination::Browser(url) => {
            open_elsewhere(&url);
            NewWindowResponse::Deny
        }
        Destination::Refused => NewWindowResponse::Deny,
    }
}

/// A navigation inside a window: another site's page goes to the browser and the window stays.
/// Everything else navigates as before, including the client's own addresses and downloads.
pub(crate) fn follows_navigation(url: &tauri::Url, origin: &Origin) -> bool {
    let Ok(client) = origin.read() else {
        return false;
    };
    match destination(url, &client) {
        Destination::Browser(url) => {
            open_elsewhere(&url);
            false
        }
        Destination::App | Destination::Refused => true,
    }
}

/// Labels have to be unique for the lifetime of the app, and a result may be opened many times.
static OPENED: AtomicUsize = AtomicUsize::new(0);

/// One result, in a window of its own: same client, same origin, its own document.
pub(crate) fn result_window<R: Runtime>(
    app: &tauri::AppHandle<R>,
    url: &str,
    features: NewWindowFeatures,
    incognito: bool,
    origin: &Origin,
) -> Result<tauri::WebviewWindow<R>, Error> {
    let address: tauri::Url = url.parse()?;
    let label = format!("result-{}", OPENED.fetch_add(1, Ordering::Relaxed));
    let closing = app.clone();
    let closed = label.clone();
    let opener = app.clone();
    let opened_from = origin.clone();
    let navigated_from = origin.clone();
    WebviewWindowBuilder::new(app, &label, WebviewUrl::External(address))
        // A window the client opened may open windows in turn: a result from the graph window,
        // a piece of a result from a result window. Each is built the same way as this one.
        .on_new_window(move |url, features| {
            requested_window(&opener, &url, features, incognito, &opened_from)
        })
        // WebKit requires its supplied configuration when returning NewWindowResponse::Create.
        .window_features(features)
        .incognito(incognito)
        .initialization_script(SHELL)
        // The page names itself after the result it is showing; the window follows it.
        .title("WesDesk")
        .on_document_title_changed(|window, title| {
            let _ = window.set_title(&title);
        })
        .inner_size(1000.0, 760.0)
        .min_inner_size(600.0, 400.0)
        .on_navigation(move |url| {
            if !is_closing(url) {
                return follows_navigation(url, &navigated_from);
            }
            // Closing from inside the navigation callback re-enters the webview; queue it instead.
            let app = closing.clone();
            let label = closed.clone();
            let _ = closing.run_on_main_thread(move || {
                if let Some(window) = app.get_webview_window(&label) {
                    let _ = window.close();
                }
            });
            false
        })
        .build()
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::{CLOSING, Destination, SHELL, destination, is_closing};

    fn url(text: &str) -> tauri::Url {
        text.parse().unwrap()
    }

    #[test]
    fn the_clients_own_pages_stay_in_the_app() {
        // Arrange
        let client = url("http://127.0.0.1:8099/");

        // Act and assert
        assert_eq!(
            destination(&url("http://127.0.0.1:8099/#open/id7/details"), &client),
            Destination::App
        );
        assert_eq!(
            destination(&url("http://127.0.0.1:8099/screen?w=x"), &client),
            Destination::App
        );
    }

    #[test]
    fn other_web_pages_go_to_the_browser() {
        // Arrange
        let client = url("http://127.0.0.1:8099/");

        // Act and assert
        for other in [
            "https://orders-api.staging.internal/docs",
            "http://example.test:8080/a?b=c",
            "http://127.0.0.1:9000/",
        ] {
            assert_eq!(
                destination(&url(other), &client),
                Destination::Browser(url(other)),
                "{other}"
            );
        }
    }

    #[test]
    fn addresses_that_are_not_plain_web_pages_are_refused() {
        // Arrange
        let client = url("http://127.0.0.1:8099/");

        // Act and assert
        for refused in [
            "https://user:secret@example.test/",
            "https://user@example.test/",
            "javascript:alert(1)",
            "file:///etc/hosts",
            "data:text/html,hello",
            "ssh://host",
            "wes://open",
        ] {
            assert_eq!(
                destination(&url(refused), &client),
                Destination::Refused,
                "{refused}"
            );
        }
    }

    #[test]
    fn the_app_origin_follows_a_data_folder_change() {
        // Arrange
        let before = url("http://127.0.0.1:8099/");
        let after = url("http://127.0.0.1:8123/");
        let page = url("http://127.0.0.1:8123/#open/id1/result");

        // Act and assert
        assert_eq!(
            destination(&page, &before),
            Destination::Browser(page.clone())
        );
        assert_eq!(destination(&page, &after), Destination::App);
    }

    #[test]
    fn only_the_shells_own_close_path_closes_a_result_window() {
        // Arrange: the address `window.close` turns into, and addresses the page may really go to.
        let closing = "http://127.0.0.1:8080/__wes/close".parse().unwrap();
        let with_query = "http://127.0.0.1:8080/__wes/close?from=result"
            .parse()
            .unwrap();
        let session = "http://127.0.0.1:8080/".parse().unwrap();
        let opened = "http://127.0.0.1:8080/#open/id7/details".parse().unwrap();
        let lookalike = "http://127.0.0.1:8080/results/__wes/close".parse().unwrap();

        // Act and assert: the path decides, and nothing else does.
        assert!(is_closing(&closing));
        assert!(is_closing(&with_query));
        assert!(!is_closing(&session));
        assert!(!is_closing(&opened));
        assert!(!is_closing(&lookalike));
    }

    /// The client calls `window.close`; only this makes that mean anything in a webview.
    #[test]
    fn every_window_is_told_it_is_the_desktop_and_how_to_close_itself() {
        assert!(SHELL.contains("window.__WES_DESKTOP__ = true"));
        assert!(SHELL.contains("window.close"));
        assert!(SHELL.contains(CLOSING));
    }
}
