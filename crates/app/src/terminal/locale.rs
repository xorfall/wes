//! The emulator speaks UTF-8 even when a desktop launcher supplies no locale.
use std::{collections::BTreeMap, ffi::OsString};

#[cfg(target_os = "macos")]
const UTF8_FALLBACK: &str = "en_US.UTF-8";
#[cfg(not(target_os = "macos"))]
const UTF8_FALLBACK: &str = "C.UTF-8";

const CATEGORIES: &[&str] = &[
    "LC_NUMERIC",
    "LC_TIME",
    "LC_COLLATE",
    "LC_MONETARY",
    "LC_MESSAGES",
    "LC_PAPER",
    "LC_NAME",
    "LC_ADDRESS",
    "LC_TELEPHONE",
    "LC_MEASUREMENT",
    "LC_IDENTIFICATION",
];

pub(super) fn environment(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> BTreeMap<OsString, OsString> {
    select(parent, available_utf8)
}

fn select(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
    available: impl FnOnce(&BTreeMap<OsString, OsString>) -> bool,
) -> BTreeMap<OsString, OsString> {
    let mut values: BTreeMap<_, _> = parent
        .into_iter()
        .filter(|(key, _)| {
            [
                "HOME", "USER", "LOGNAME", "LANG", "LC_CTYPE", "LC_ALL", "TMPDIR",
            ]
            .iter()
            .chain(CATEGORIES.iter())
            .any(|allowed| key == allowed)
        })
        .collect();
    let effective = ["LC_ALL", "LC_CTYPE", "LANG"].iter().find_map(|key| {
        values
            .get(&OsString::from(key))
            .filter(|value| !value.is_empty())
    });
    let utf8 = effective
        .and_then(|value| value.to_str())
        .is_some_and(|locale| {
            let normalized = locale.to_ascii_lowercase().replace('-', "");
            normalized
                .split('@')
                .next()
                .is_some_and(|name| name.ends_with(".utf8") || name == "utf8")
        });
    if !utf8 || !available(&values) {
        // Expand LC_ALL before replacing only its character category. An ASCII shell must
        // not corrupt UTF-8 input, but number/date/collation settings remain effective.
        if let Some(all) = values
            .remove(&OsString::from("LC_ALL"))
            .filter(|value| !value.is_empty())
        {
            for category in CATEGORIES {
                values.insert((*category).into(), all.clone());
            }
        }
        values.insert("LC_CTYPE".into(), UTF8_FALLBACK.into());
    }
    if values
        .get(&OsString::from("LANG"))
        .is_none_or(|value| value.is_empty())
    {
        values.insert("LANG".into(), UTF8_FALLBACK.into());
    }
    values
}

// Check the platform's actual character encoding, not merely a locale-name suffix.
// This fixed local tool runs without shell startup files, inherited secrets or locale-path overrides.
fn available_utf8(values: &BTreeMap<OsString, OsString>) -> bool {
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let Ok(mut child) = wes_adapters::process::serialized_spawn(|| {
        Command::new("/usr/bin/locale")
            .arg("charmap")
            .env_clear()
            .envs(values.iter().filter(|(key, _)| {
                key.to_str()
                    .is_some_and(|name| name == "LANG" || name.starts_with("LC_"))
            }))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
    }) else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_millis(250);
    let valid = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let mut output = String::new();
    valid
        && child
            .stdout
            .take()
            .is_some_and(|out| out.take(64).read_to_string(&mut output).is_ok())
        && output.trim().eq_ignore_ascii_case("UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn selected(entries: &[(&str, &str)]) -> BTreeMap<OsString, OsString> {
        select(
            entries
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into())),
            |_| true,
        )
    }
    #[test]
    fn desktop_without_locale_and_explicit_ascii_get_utf8() {
        for entries in [
            &[][..],
            &[("LANG", "C")],
            &[("LANG", ""), ("LC_CTYPE", "POSIX")],
        ] {
            let values = selected(entries);
            assert_eq!(values[&OsString::from("LC_CTYPE")], UTF8_FALLBACK);
        }
        assert_eq!(selected(&[])[&OsString::from("LANG")], UTF8_FALLBACK);
    }
    #[test]
    fn existing_utf8_selection_obeys_locale_precedence() {
        for entries in [
            vec![("LANG", "tr_TR.UTF-8")],
            vec![("LANG", "C"), ("LC_CTYPE", "tr_TR.utf8")],
            vec![("LANG", "C"), ("LC_CTYPE", "C"), ("LC_ALL", "tr_TR.UTF-8")],
        ] {
            let values = selected(&entries);
            for (key, value) in entries {
                assert_eq!(values[&OsString::from(key)], value);
            }
        }
        let values = selected(&[("LANG", "tr_TR.UTF-8"), ("LC_CTYPE", "C")]);
        assert_eq!(values[&OsString::from("LC_CTYPE")], UTF8_FALLBACK);
        let values = selected(&[("LC_CTYPE", "tr_TR.UTF-8"), ("LC_ALL", "C")]);
        assert!(!values.contains_key(&OsString::from("LC_ALL")));
        assert_eq!(values[&OsString::from("LC_CTYPE")], UTF8_FALLBACK);
        assert_eq!(values[&OsString::from("LC_NUMERIC")], "C");
    }
    #[test]
    fn unavailable_utf8_locale_falls_back_without_replacing_other_categories() {
        let values = select(
            [
                ("LANG".into(), "missing.UTF-8".into()),
                ("LC_NUMERIC".into(), "C".into()),
            ],
            |_| false,
        );
        assert_eq!(values[&OsString::from("LC_CTYPE")], UTF8_FALLBACK);
        assert_eq!(values[&OsString::from("LANG")], "missing.UTF-8");
        assert_eq!(values[&OsString::from("LC_NUMERIC")], "C");
        let values = selected(&[("LC_ALL", "C"), ("LC_NUMERIC", "tr_TR.UTF-8")]);
        assert_eq!(values[&OsString::from("LC_NUMERIC")], "C");
    }
    #[test]
    fn unrelated_host_values_do_not_enter_terminal_environment() {
        let values = selected(&[
            ("HOME", "/synthetic/home"),
            ("SECRET_TOKEN", "private"),
            ("BASH_ENV", "/untrusted"),
        ]);
        assert_eq!(values[&OsString::from("HOME")], "/synthetic/home");
        assert!(!values.contains_key(&OsString::from("SECRET_TOKEN")));
        assert!(!values.contains_key(&OsString::from("BASH_ENV")));
    }
}
