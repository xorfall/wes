//! Native text input policy. Cocoa's hold-to-select-accent behavior consumes letter
//! repeats before WKWebView/xterm can receive them. Configure only this app's defaults;
//! keyboard repeat rate/delay and the user's global input preferences remain owned by macOS.

pub(crate) fn configure() {
    #[cfg(target_os = "macos")]
    enable_repeat(&objc2_foundation::NSUserDefaults::standardUserDefaults());
}

#[cfg(target_os = "macos")]
fn enable_repeat(defaults: &objc2_foundation::NSUserDefaults) {
    defaults.setBool_forKey(
        false,
        objc2_foundation::ns_string!("ApplePressAndHoldEnabled"),
    );
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::enable_repeat;
    use objc2::AnyThread;
    use objc2_foundation::{NSString, NSUserDefaults, ns_string};

    #[test]
    fn held_letters_repeat_without_changing_other_app_or_keyboard_preferences() {
        // Unique Foundation suites, never standardUserDefaults or the user's app domain.
        let directory = tempfile::tempdir().unwrap();
        let name = NSString::from_str(&format!(
            "app.wes.test.keyboard.{}",
            directory.path().file_name().unwrap().to_string_lossy()
        ));
        let other_name = NSString::from_str(&format!("{name}.other"));
        let app = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&name)).unwrap();
        let other =
            NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(&other_name)).unwrap();
        struct Cleanup<'a>(&'a NSUserDefaults, &'a NSString, &'a NSString);
        impl Drop for Cleanup<'_> {
            fn drop(&mut self) {
                self.0.removePersistentDomainForName(self.1);
                self.0.removePersistentDomainForName(self.2);
            }
        }
        let _cleanup = Cleanup(&app, &name, &other_name);
        let key = ns_string!("ApplePressAndHoldEnabled");
        other.setBool_forKey(true, key);
        app.setInteger_forKey(2, ns_string!("KeyRepeat"));
        app.setInteger_forKey(25, ns_string!("InitialKeyRepeat"));
        app.setBool_forKey(true, ns_string!("unrelatedPreference"));

        // Absent app override and then an explicitly enabled accent picker both get repeat.
        enable_repeat(&app);
        assert!(!app.boolForKey(key));
        app.setBool_forKey(true, key);
        enable_repeat(&app);
        enable_repeat(&app);
        assert!(!app.boolForKey(key));
        assert!(app.objectForKey(key).is_some());
        assert!(other.boolForKey(key));
        assert_eq!(app.integerForKey(ns_string!("KeyRepeat")), 2);
        assert_eq!(app.integerForKey(ns_string!("InitialKeyRepeat")), 25);
        assert!(app.boolForKey(ns_string!("unrelatedPreference")));
    }
}

/// Cmd+M belongs to cell actions; macOS otherwise consumes it as Minimize before WebKit.
#[cfg(target_os = "macos")]
pub(crate) fn menu<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> tauri::Result<tauri::menu::Menu<R>> {
    let menu = tauri::menu::Menu::default(app)?;
    {
        use tauri::menu::{MenuItem, Submenu, WINDOW_SUBMENU_ID};
        if let Some(window) = menu
            .get(WINDOW_SUBMENU_ID)
            .and_then(|item| item.as_submenu().cloned())
        {
            // The framework's default Window menu starts with the native Minimize item.
            if let Some(first) = window.remove_at(0)? {
                if let tauri::menu::MenuItemKind::Predefined(item) = first {
                    window.insert(
                        &MenuItem::with_id(
                            app,
                            "wes-window-minimize",
                            item.text()?,
                            true,
                            None::<&str>,
                        )?,
                        0,
                    )?;
                } else {
                    window.insert(&first, 0)?;
                }
            }
        }
        menu.append(&Submenu::with_items(
            app,
            "Cell",
            true,
            &[&MenuItem::with_id(
                app,
                "wes-cell-actions",
                "Cell actions",
                true,
                Some("CmdOrCtrl+M"),
            )?],
        )?)?;
    }
    Ok(menu)
}

#[cfg(target_os = "macos")]
pub(crate) fn menu_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event: tauri::menu::MenuEvent,
) {
    use tauri::Manager;
    if !matches!(
        event.id().as_ref(),
        "wes-cell-actions" | "wes-window-minimize"
    ) {
        return;
    }
    if let Some(window) = app
        .webview_windows()
        .into_values()
        .find(|window| window.is_focused().unwrap_or(false))
    {
        if event.id().as_ref() == "wes-window-minimize" {
            let _ = window.minimize();
        } else {
            // Send the ordinary UI shortcut; the focused cell retains its editable-target and action checks.
            let _ = window.eval("document.activeElement?.dispatchEvent(new KeyboardEvent('keydown',{key:'m',metaKey:true,bubbles:true,cancelable:true}))");
        }
    }
}
