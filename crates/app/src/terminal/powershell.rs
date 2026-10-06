//! Windows PowerShell 5.1 as the local shell. The user's profile, modules and execution policy
//! are neither read nor changed; a newer PowerShell is not required.
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};
use wes_adapters::execution_targets::HostLaunch;

/// Read into a script block rather than run as a file: a script file would need an execution
/// policy this terminal has no business loosening, and the default policy refuses every file.
const LOADER: &str = ". ([scriptblock]::Create([IO.File]::ReadAllText($env:WES_BOOTSTRAP)))";

/// What Windows programs need to start and find the user's folders. PSModulePath is left to
/// the shell: one inherited from a newer PowerShell would load that version's modules.
const INHERITED: &[&str] = &[
    "SystemRoot",
    "SystemDrive",
    "windir",
    "ComSpec",
    "PATH",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "HOME",
    "USERNAME",
    "USERDOMAIN",
    "COMPUTERNAME",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ALLUSERSPROFILE",
    "PUBLIC",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
];

pub(super) fn environment(
    parent: impl IntoIterator<Item = (OsString, OsString)>,
) -> BTreeMap<OsString, OsString> {
    let mut values: BTreeMap<_, _> = parent
        .into_iter()
        .filter(|(key, _)| {
            key.to_str()
                .is_some_and(|key| INHERITED.iter().any(|name| name.eq_ignore_ascii_case(key)))
        })
        .collect();
    // The shell finds a program by these extensions; the workspace commands are programs.
    let extensions = values
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATHEXT"))
        .map(|(key, value)| (key.clone(), value.clone()));
    match extensions {
        None => {
            values.insert("PATHEXT".into(), ".COM;.EXE;.BAT;.CMD".into());
        }
        Some((key, value)) => {
            let listed = value
                .to_string_lossy()
                .split(';')
                .any(|extension| extension.eq_ignore_ascii_case(".EXE"));
            if !listed {
                let mut extended = value;
                extended.push(";.EXE");
                values.insert(key, extended);
            }
        }
    }
    values
}

/// The system's own copy by absolute path; a PATH lookup could select another program.
fn executable(system_root: Option<&OsStr>) -> io::Result<PathBuf> {
    system_root
        .map(Path::new)
        .filter(|root| root.is_absolute())
        .map(|root| root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
        .filter(|shell| shell.is_file())
        .ok_or_else(|| io::Error::other("Windows PowerShell was not found on this host."))
}

pub(super) fn launch(directory: &Path) -> io::Result<HostLaunch> {
    let directory = directory.join("prompt");
    std::fs::create_dir(&directory)?;
    let bootstrap = directory.join("prompt.ps1");
    std::fs::write(&bootstrap, include_str!("prompt.ps1"))?;
    let mut command = HostLaunch {
        executable: executable(std::env::var_os("SystemRoot").as_deref())?,
        ..Default::default()
    };
    for argument in ["-NoLogo", "-NoProfile", "-NoExit", "-Command", LOADER] {
        command.arg(argument);
    }
    command.env("WES_BOOTSTRAP", bootstrap);
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrelated_host_values_do_not_enter_the_shell_and_names_ignore_case() {
        let values = environment(
            [
                ("systemroot", r"C:\Windows"),
                ("Path", r"C:\tools"),
                ("SECRET_TOKEN", "private"),
                ("PSModulePath", r"C:\newer\Modules"),
                ("PSExecutionPolicyPreference", "Bypass"),
            ]
            .map(|(key, value)| (key.into(), value.into())),
        );
        assert_eq!(values[&OsString::from("systemroot")], r"C:\Windows");
        assert_eq!(values[&OsString::from("Path")], r"C:\tools");
        assert_eq!(values[&OsString::from("PATHEXT")], ".COM;.EXE;.BAT;.CMD");
        assert_eq!(values.len(), 3);
        let narrowed = environment([("PathExt".into(), ".BAT;.cmd".into())]);
        assert_eq!(narrowed[&OsString::from("PathExt")], ".BAT;.cmd;.EXE");
        let listed = environment([("PATHEXT".into(), ".com;.exe".into())]);
        assert_eq!(listed[&OsString::from("PATHEXT")], ".com;.exe");
    }

    #[test]
    fn the_shell_is_the_systems_own_and_nothing_loosens_its_policy() {
        assert!(executable(None).is_err());
        assert!(executable(Some(OsStr::new("relative"))).is_err());
        let shell = executable(std::env::var_os("SystemRoot").as_deref()).unwrap();
        assert!(shell.is_absolute() && shell.ends_with("powershell.exe"));
        let directory = tempfile::tempdir().unwrap();
        let command = launch(directory.path()).unwrap();
        let arguments: Vec<_> = command
            .arguments
            .iter()
            .map(|argument| argument.to_string_lossy().to_ascii_lowercase())
            .collect();
        assert!(arguments.contains(&"-noprofile".into()));
        assert!(arguments.iter().all(|a| !a.contains("executionpolicy")
            && !a.starts_with("-file")
            && !a.contains("bypass")));
        assert!(directory.path().join("prompt/prompt.ps1").is_file());
    }
}
