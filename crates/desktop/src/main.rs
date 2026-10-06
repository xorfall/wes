//! Native desktop shell: one window over the in-process engine and its loopback server.
//! The window's origin is the server origin itself, so the engine's same-origin loopback
//! policy applies unchanged. Desktop UI preferences use the same server, outside engine history.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};
use wes::data_home::host::DesktopHost;

mod debug;
mod keyboard;
mod windows;
use windows::{Origin, SHELL, follows_navigation, requested_window};

/// The host owns every opened engine and joins them at exit.
type Held = Mutex<Option<DesktopHost>>;

fn main() {
    if let Some(code) = wes::view_toolchain::entry() {
        std::process::exit(i32::from(code));
    }
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    if let Some(code) = tokio.block_on(wes::terminal::assistant::entry()) {
        std::process::exit(i32::from(code));
    }
    if let Some(code) = tokio.block_on(wes::terminal::client()) {
        std::process::exit(i32::from(code));
    }
    tauri::async_runtime::set(tokio.handle().clone());
    keyboard::configure();
    let builder = tauri::Builder::default();
    // Only macOS has an application menu bar. Elsewhere a menu would be drawn inside the window
    // and its accelerators would take shortcuts such as Ctrl+C from the terminal and editors.
    #[cfg(target_os = "macos")]
    let builder = builder
        .menu(keyboard::menu)
        .on_menu_event(keyboard::menu_event);
    let app = builder
        .setup(|app| {
            let paths = debug::Paths::resolve(
                cfg!(debug_assertions),
                std::env::var_os("WES_DEBUG_HOME"),
                std::env::home_dir(),
                app.path().app_config_dir().ok(),
            )?;
            wes::budgets::initialize(&paths.user_home)?;
            let isolated = paths.isolated;
            if let Some(directory) = paths.diagnostics {
                if let Some(guard) = wes::telemetry::install(directory) {
                    app.manage(Mutex::new(Some(guard)));
                }
            }
            let startup = wes::diagnostics_startup();
            let resources = app.path().resource_dir().ok();
            if let Some(resources) = &resources {
                wes::api_library::use_bundled_resources(resources.clone());
                wes::view_toolchain::use_bundled_resources(resources.clone());
            }
            let site = site_directory(resources.as_deref());
            let host =
                tauri::async_runtime::block_on(DesktopHost::start(paths.user_home, Some(site)))
                    .map_err(|error| -> Box<dyn std::error::Error> { error })?;
            startup.finish("ok");
            let url: tauri::Url = host.location().url.parse().expect("loopback server URL");
            let mut locations = host.subscribe();
            app.manage::<Held>(Mutex::new(Some(host)));
            let opener = app.handle().clone();
            let origin: Origin = std::sync::Arc::new(std::sync::RwLock::new(url.clone()));
            let opened_from = origin.clone();
            let navigated_from = origin.clone();
            let changed_origin = origin.clone();
            WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .initialization_script(SHELL)
                .title(if isolated {
                    "WesDesk — Rust Debug"
                } else {
                    "WesDesk"
                })
                .incognito(isolated)
                .inner_size(1280.0, 860.0)
                .min_inner_size(720.0, 480.0)
                /*
                 * A result opened from the session gets a separate app window rather than
                 * replacing the session or opening in whatever the machine calls a browser. The
                 * session's own window is not navigated at all, so it keeps its scrollback and its
                 * half-typed line.
                 */
                .on_new_window(move |url, features| {
                    requested_window(&opener, &url, features, isolated, &opened_from)
                })
                // A link to another site leaves for the browser instead of replacing the session.
                .on_navigation(move |url| follows_navigation(url, &navigated_from))
                .build()?;
            let shell = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                while locations.changed().await.is_ok() {
                    let location = locations.borrow_and_update().clone();
                    let app = shell.clone();
                    let origin = changed_origin.clone();
                    let _ = shell.run_on_main_thread(move || {
                        // Every pane belongs to the opened home. Result windows from the old
                        // engine must not keep a stale origin or editable credential controls.
                        for (label, window) in app.webview_windows() {
                            if label != "main" {
                                let _ = window.close();
                            }
                        }
                        if let Some(window) = app.get_webview_window("main") {
                            if let Ok(url) = location.url.parse::<tauri::Url>() {
                                // The new home's address is the client's own before it is loaded.
                                if let Ok(mut client) = origin.write() {
                                    *client = url.clone();
                                }
                                if let Err(error) = window.navigate(url) {
                                    eprintln!("wes-desktop: data-folder navigation: {error}");
                                    tracing::warn!(target: "wes.telemetry", notice = "ui_error");
                                }
                            }
                        }
                    });
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .unwrap_or_else(|error| {
            eprintln!("wes-desktop: startup failed: {error}");
            std::process::exit(1);
        });
    app.run(|app, event| {
        if matches!(event, RunEvent::Exit) {
            let backend = app.state::<Held>().lock().expect("backend state").take();
            if let Some(host) = backend {
                tauri::async_runtime::block_on(async move {
                    if let Err(error) = host.shutdown().await {
                        eprintln!("wes-desktop: shutdown: {error}");
                    }
                });
            }
            if let Some(state) = app.try_state::<Mutex<Option<wes::telemetry::Guard>>>() {
                state.lock().expect("telemetry guard").take();
            }
        }
    });
}

/// Prefer the bundled client; fall back to the repository build for `cargo run` development.
fn site_directory(resources: Option<&Path>) -> PathBuf {
    if let Some(resources) = resources {
        let bundled = resources.join("gui-dist");
        if bundled.join("index.html").is_file() {
            return bundled;
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../gui/dist")
}

#[cfg(test)]
mod tests {
    use super::site_directory;

    #[test]
    fn bundled_client_is_preferred_and_repository_build_is_the_fallback() {
        // Arrange: one resource directory with a bundled client, one without.
        let bundled = tempfile::tempdir().unwrap();
        std::fs::create_dir(bundled.path().join("gui-dist")).unwrap();
        std::fs::write(bundled.path().join("gui-dist/index.html"), "client").unwrap();
        let empty = tempfile::tempdir().unwrap();

        // Act.
        let preferred = site_directory(Some(bundled.path()));
        let fallback = site_directory(Some(empty.path()));
        let missing = site_directory(None);

        // Assert: only a resource directory holding index.html wins over gui/dist.
        assert_eq!(preferred, bundled.path().join("gui-dist"));
        assert!(fallback.ends_with("gui/dist"));
        assert!(missing.ends_with("gui/dist"));
    }
}
