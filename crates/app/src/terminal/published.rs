//! Workspace commands as real programs in a Windows pane. Each one is this executable under
//! the command's name, so arguments arrive exactly as the caller passed them: no batch file
//! and no `cmd.exe` stands between a caller's argument and the bridge.
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

const BASE: &str = "wes-provider.exe";
const CONTROL: &[&str] = &["wes-provider", "wes-value", "wes-mcp", "wesx"];

/// A provider name that may not become a program: Windows matches file names without regard
/// to case, so every spelling of a fixed command is the fixed command.
fn reserved(name: &str) -> bool {
    CONTROL
        .iter()
        .any(|control| control.eq_ignore_ascii_case(name))
        || device(name)
}
/// Names Windows reserves for devices in every directory, with or without an extension.
fn device(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (name.len() == 4
            && (name.starts_with("COM") || name.starts_with("LPT"))
            && name.as_bytes()[3].is_ascii_digit())
}

fn link(base: &Path, to: &Path) -> io::Result<()> {
    match std::fs::hard_link(base, to) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => Err(error),
        _ => Ok(()),
    }
}

/// The fixed commands. A link shares the executable's storage, but Windows links files only
/// within one volume. When the executable lives on another volume than the pane's directory
/// it is copied once, and every later name links to that copy.
pub(super) fn prepare(directory: &Path, executable: &Path) -> io::Result<()> {
    prepare_with(directory, executable, |from, to| {
        std::fs::hard_link(from, to)
    })
}
fn prepare_with(
    directory: &Path,
    executable: &Path,
    first: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let base = directory.join(BASE);
    if first(executable, &base).is_err() {
        std::fs::copy(executable, &base)?;
    }
    link(&base, &directory.join("wes-value.exe"))?;
    link(&base, &directory.join("assistants").join("wesx.exe"))
}
/// A further program in the control directory, such as an assistant client's launcher.
pub(super) fn control(directory: &Path, name: &str) -> io::Result<()> {
    link(
        &directory.join(BASE),
        &directory.join("assistants").join(format!("{name}.exe")),
    )
}

/// One command per provider. Windows cannot tell two names apart by case, so providers that
/// differ only by case are published under neither name rather than answering for each other.
pub(super) fn aliases(directory: &Path, names: Vec<String>) -> io::Result<()> {
    let mut folded: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in names.into_iter().take(1024) {
        if super::alias_name(&name) && !reserved(&name) {
            let spellings = folded.entry(name.to_ascii_lowercase()).or_default();
            if !spellings.contains(&name) {
                spellings.push(name);
            }
        }
    }
    let base = directory.join(BASE);
    for spellings in folded.into_values() {
        let path = directory.join(format!("{}.exe", spellings[0]));
        if spellings.len() == 1 {
            link(&base, &path)?;
        } else {
            match std::fs::remove_file(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
    }
    Ok(())
}

/// The control directory first, the provider commands last, the host's own search path between.
pub(super) fn path(inherited: Option<&OsStr>, directory: &Path) -> io::Result<OsString> {
    let mut paths = vec![directory.join("assistants")];
    if let Some(inherited) = inherited.filter(|value| !value.is_empty()) {
        paths.extend(std::env::split_paths(inherited).filter(|path| path.is_absolute()));
    }
    paths.push(directory.to_owned());
    std::env::join_paths(paths).map_err(io::Error::other)
}

/// The command this process was started as, when it is one of a pane's published programs,
/// and whether it came from the control directory. The name is the directory's own spelling:
/// a caller may type it in any case.
pub(super) fn tool() -> Option<(String, bool)> {
    let directory = std::fs::canonicalize(std::env::var_os("WES_ASSISTANT_DIRECTORY")?).ok()?;
    let executable = std::env::current_exe().ok()?;
    let parent = std::fs::canonicalize(executable.parent()?).ok()?;
    let control = parent == directory.join("assistants");
    if parent != directory && !control {
        return None;
    }
    let started = executable.file_name()?.to_str()?;
    std::fs::read_dir(&parent)
        .ok()?
        .flatten()
        .find_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            name.eq_ignore_ascii_case(started)
                .then(|| PathBuf::from(name).file_stem()?.to_str().map(str::to_owned))?
                .map(|name| (name, control))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepared() -> tempfile::TempDir {
        let directory = tempfile::Builder::new()
            .prefix("boş luk ")
            .tempdir()
            .unwrap();
        std::fs::create_dir(directory.path().join("assistants")).unwrap();
        prepare(directory.path(), &std::env::current_exe().unwrap()).unwrap();
        directory
    }
    fn programs(directory: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn fixed_commands_are_programs_and_never_batch_files() {
        let directory = prepared();
        assert_eq!(
            programs(directory.path()),
            ["assistants", "wes-provider.exe", "wes-value.exe"]
        );
        assert_eq!(programs(&directory.path().join("assistants")), ["wesx.exe"]);
        assert_eq!(
            std::fs::metadata(directory.path().join("wes-value.exe"))
                .unwrap()
                .len(),
            std::fs::metadata(std::env::current_exe().unwrap())
                .unwrap()
                .len()
        );
    }

    #[test]
    fn an_executable_that_cannot_be_linked_is_copied_once_and_the_names_link_to_the_copy() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("assistants")).unwrap();
        let executable = std::env::current_exe().unwrap();
        prepare_with(directory.path(), &executable, |_, _| {
            Err(io::ErrorKind::CrossesDevices.into())
        })
        .unwrap();
        aliases(directory.path(), vec!["http".into()]).unwrap();
        control(directory.path(), "claude").unwrap();
        let size = std::fs::metadata(&executable).unwrap().len();
        for program in [
            "wes-provider.exe",
            "wes-value.exe",
            "http.exe",
            r"assistants\wesx.exe",
            r"assistants\claude.exe",
        ] {
            assert_eq!(
                std::fs::metadata(directory.path().join(program))
                    .unwrap()
                    .len(),
                size,
                "{program}"
            );
        }
    }

    /// The same, on a machine that has a second volume: the executable really is elsewhere.
    #[test]
    fn an_executable_on_another_volume_is_published_by_copy() {
        let pane = tempfile::tempdir().unwrap();
        let Some(elsewhere) = ('A'..='Z')
            .map(|letter| PathBuf::from(format!(r"{letter}:\")))
            .filter(|root| !pane.path().starts_with(root))
            .find_map(|root| {
                tempfile::Builder::new()
                    .prefix("wes-volume-")
                    .tempdir_in(root)
                    .ok()
            })
        else {
            eprintln!("no second writable volume on this machine; covered by the injected case");
            return;
        };
        let executable = elsewhere.path().join("wes elsewhere.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        assert!(
            std::fs::hard_link(&executable, pane.path().join("probe.exe")).is_err(),
            "the volumes do not differ"
        );
        std::fs::create_dir(pane.path().join("assistants")).unwrap();
        prepare(pane.path(), &executable).unwrap();
        aliases(pane.path(), vec!["http".into()]).unwrap();
        // The published program starts and reports itself: it is a complete copy.
        let output = std::process::Command::new(pane.path().join("http.exe"))
            .args(["--list", "--exact", "no-such-test"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    #[test]
    fn provider_names_skip_devices_controls_and_spellings_windows_cannot_separate() {
        let directory = prepared();
        let names = |list: &[&str]| list.iter().map(|name| (*name).to_owned()).collect();
        aliases(
            directory.path(),
            names(&[
                "http", "lab-1", "nul", "COM3", "wesx", "wes-mcp", "../up", "a.b", "", "Git",
            ]),
        )
        .unwrap();
        assert_eq!(
            programs(directory.path()),
            [
                "Git.exe",
                "assistants",
                "http.exe",
                "lab-1.exe",
                "wes-provider.exe",
                "wes-value.exe"
            ]
        );
        // A provider that appears later and differs only by case withdraws the shared name.
        aliases(directory.path(), names(&["http", "Git", "git"])).unwrap();
        assert!(!directory.path().join("Git.exe").exists());
        assert!(directory.path().join("http.exe").exists());
        // Repeating the catalogue is the ordinary refresh and changes nothing.
        aliases(directory.path(), names(&["http"])).unwrap();
    }

    #[test]
    fn no_spelling_of_a_fixed_command_is_published_or_removed_as_a_provider() {
        let directory = prepared();
        let size = std::fs::metadata(std::env::current_exe().unwrap())
            .unwrap()
            .len();
        let fixed = |directory: &Path| {
            for program in ["wes-provider.exe", "wes-value.exe", r"assistants\wesx.exe"] {
                assert_eq!(
                    std::fs::metadata(directory.join(program)).unwrap().len(),
                    size,
                    "{program}"
                );
            }
        };
        let names = |list: &[&str]| list.iter().map(|name| (*name).to_owned()).collect();
        // One spelling, and spellings that differ only by case, of every fixed command.
        for providers in [
            &["WES-VALUE"][..],
            &["WES-PROVIDER", "Wes-Provider"],
            &["Wes-Value", "wes-value", "WES-VALUE"],
            &["WESX", "WesX", "Wes-Mcp", "WES-MCP", "wes-provider"],
        ] {
            aliases(directory.path(), names(providers)).unwrap();
            fixed(directory.path());
            assert_eq!(
                programs(directory.path()),
                ["assistants", "wes-provider.exe", "wes-value.exe"]
            );
            assert_eq!(programs(&directory.path().join("assistants")), ["wesx.exe"]);
        }
        // Ordinary providers beside them are still published.
        aliases(
            directory.path(),
            names(&["WES-PROVIDER", "Wes-Provider", "http"]),
        )
        .unwrap();
        fixed(directory.path());
        assert!(directory.path().join("http.exe").exists());
        for name in [
            "wes-provider",
            "WES-PROVIDER",
            "Wes-Value",
            "wesX",
            "WES-MCP",
            "nul",
            "Com1",
        ] {
            assert!(reserved(name), "{name}");
        }
        assert!(!reserved("wes-providers") && !reserved("http"));
    }

    #[test]
    fn the_search_path_keeps_host_programs_ahead_of_provider_names() {
        let value = path(
            Some(OsStr::new(r"C:\Program Files\Tool;relative;C:\Üser\bin")),
            Path::new(r"C:\pane"),
        )
        .unwrap();
        let paths: Vec<_> = std::env::split_paths(&value).collect();
        assert_eq!(
            paths,
            [
                r"C:\pane\assistants",
                r"C:\Program Files\Tool",
                r"C:\Üser\bin",
                r"C:\pane"
            ]
            .map(PathBuf::from)
        );
        assert_eq!(
            std::env::split_paths(&path(None, Path::new(r"C:\pane")).unwrap()).count(),
            2
        );
    }
}
