//! Keep descriptor preparation and child creation together across owned launches.

#[cfg(target_os = "macos")]
static LAUNCH: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// macOS creates stdio pipes before setting close-on-exec. Concurrent child creation
/// in that interval can inherit another launch's pipe and prevent EOF. Protect only
/// synchronous preparation/spawn, never child I/O, waiting or external effects.
pub fn serialized_spawn<T>(launch: impl FnOnce() -> T) -> T {
    #[cfg(target_os = "macos")]
    let _guard = {
        LAUNCH
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    launch()
}

/// Async callers wait without occupying a runtime worker. The closure still runs
/// exactly once, after acquiring the same gate used by synchronous launches.
pub async fn serialized_spawn_async<T>(launch: impl FnOnce() -> T) -> T {
    #[cfg(target_os = "macos")]
    loop {
        match LAUNCH.try_lock() {
            Ok(_guard) => return launch(),
            Err(std::sync::TryLockError::Poisoned(error)) => {
                let _guard = error.into_inner();
                return launch();
            }
            Err(std::sync::TryLockError::WouldBlock) => (),
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
    #[cfg(not(target_os = "macos"))]
    launch()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::{
        io::Read,
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };

    #[test]
    fn concurrent_launch_cannot_inherit_a_pipe_before_cloexec_is_set() {
        let (pipe_tx, pipe_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first = std::thread::spawn(move || {
            serialized_spawn(|| {
                let (reader, writer) = std::io::pipe().unwrap();
                // Model the interval between pipe() and fcntl(CLOEXEC) deterministically.
                rustix::io::fcntl_setfd(&writer, rustix::io::FdFlags::empty()).unwrap();
                pipe_tx.send(reader).unwrap();
                release_rx.recv().unwrap();
                rustix::io::fcntl_setfd(&writer, rustix::io::FdFlags::CLOEXEC).unwrap();
                writer
            })
        });
        let mut reader = pipe_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (child_tx, child_rx) = mpsc::channel();
        let second = std::thread::spawn(move || {
            let child = serialized_spawn(|| {
                Command::new("/bin/cat")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            })
            .unwrap();
            child_tx.send(child).unwrap();
        });
        let early = child_rx.recv_timeout(Duration::from_millis(250)).ok();
        release_tx.send(()).unwrap();
        let writer = first.join().unwrap();
        let mut child =
            early.unwrap_or_else(|| child_rx.recv_timeout(Duration::from_secs(5)).unwrap());
        second.join().unwrap();
        drop(writer);
        let (eof_tx, eof_rx) = mpsc::channel();
        let reading = std::thread::spawn(move || {
            let _ = eof_tx.send(reader.read(&mut [0]));
        });
        let eof = eof_rx.recv_timeout(Duration::from_millis(500));
        // Always terminate the owned synthetic child before asserting or joining a reader.
        child.kill().unwrap();
        child.wait().unwrap();
        reading.join().unwrap();
        assert!(
            matches!(eof, Ok(Ok(0))),
            "another child inherited the pipe writer: {eof:?}"
        );
    }
    #[tokio::test]
    async fn waiting_launch_can_be_cancelled_without_blocking_the_runtime() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let owner = std::thread::spawn(move || {
            serialized_spawn(|| {
                ready_tx.send(()).unwrap();
                // Bound cleanup even if a future regression blocks the timer's worker.
                let _ = release_rx.recv_timeout(Duration::from_secs(2));
            })
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let admitted = ran.clone();
        let result = tokio::time::timeout(
            Duration::from_millis(20),
            serialized_spawn_async(move || admitted.store(true, Ordering::SeqCst)),
        )
        .await;
        let _ = release_tx.send(());
        owner.join().unwrap();
        assert!(
            result.is_err(),
            "waiting for launch blocked the runtime timer"
        );
        assert!(
            !ran.load(Ordering::SeqCst),
            "a cancelled wait launched its operation"
        );
    }
}

/// The working directory of a native local launch, resolved once when its binding is built.
///
/// A declared directory always wins and must be an absolute host path. A target that declares
/// none starts at the root of the file system: `/` on Unix, and on Windows the root of the
/// volume the system lives on, since every volume there has a root of its own. It is never
/// the host's current directory, the user's home or the data home, which change under a
/// running engine or expose more than a launch was given.
///
/// # Errors
/// Returns why no directory can be used; nothing is launched from a directory that was guessed.
pub(crate) fn local_directory(declared: Option<&str>) -> Result<std::path::PathBuf, &'static str> {
    match declared {
        Some(directory) if std::path::Path::new(directory).is_absolute() => Ok(directory.into()),
        Some(_) => Err("Local target cwd must be an absolute host path"),
        None => default_directory(),
    }
}
#[cfg(not(windows))]
fn default_directory() -> Result<std::path::PathBuf, &'static str> {
    Ok("/".into())
}
#[cfg(windows)]
fn default_directory() -> Result<std::path::PathBuf, &'static str> {
    system_volume_root(std::env::var_os("SystemRoot").as_deref())
}
/// The root of the volume that holds the system directory, as the system itself reports it.
/// No drive letter is assumed, and a value that is not a fully absolute existing directory is
/// not repaired into one.
#[cfg(any(windows, test))]
fn system_volume_root(
    system_directory: Option<&std::ffi::OsStr>,
) -> Result<std::path::PathBuf, &'static str> {
    const UNRESOLVED: &str = "The system volume root could not be resolved for a local target without cwd; declare an absolute cwd";
    let system_directory = std::path::Path::new(system_directory.ok_or(UNRESOLVED)?);
    if !system_directory.is_absolute() {
        return Err(UNRESOLVED);
    }
    let root = system_directory.ancestors().last().ok_or(UNRESOLVED)?;
    if root.is_absolute() && root.is_dir() {
        Ok(root.to_owned())
    } else {
        Err(UNRESOLVED)
    }
}

#[cfg(test)]
mod directory_tests {
    use super::*;

    #[test]
    fn a_declared_directory_wins_and_must_be_absolute_on_this_host() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(local_directory(here.to_str()), Ok(here));
        for relative in ["relative", "./here", "", "..", "C:relative"] {
            assert!(local_directory(Some(relative)).is_err(), "{relative:?}");
        }
        // Rooted without a drive is absolute on Unix and is not on Windows; neither is widened.
        assert_eq!(local_directory(Some("/tmp")).is_ok(), cfg!(unix));
    }

    #[test]
    fn an_undeclared_directory_is_the_file_system_root_as_an_absolute_path() {
        let root = local_directory(None).unwrap();
        assert!(root.is_absolute() && root.is_dir(), "{root:?}");
        assert_eq!(root.parent(), None);
        #[cfg(unix)]
        assert_eq!(root, std::path::Path::new("/"));
        #[cfg(windows)]
        {
            // The volume the system reports, whatever its letter; never a fixed one.
            let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
            assert!(system.starts_with(&root), "{system:?} under {root:?}");
            assert_eq!(root.components().count(), 2);
        }
    }

    #[test]
    fn an_unusable_system_directory_resolves_to_nothing_instead_of_a_guess() {
        use std::ffi::OsStr;
        for unusable in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("Windows")),
            Some(OsStr::new(r"\Windows")),
            Some(OsStr::new("/Windows")).filter(|_| cfg!(windows)),
            Some(OsStr::new(r"Q:Windows")),
        ] {
            if unusable.is_none() && cfg!(not(windows)) {
                continue;
            }
            assert!(system_volume_root(unusable).is_err(), "{unusable:?}");
        }
        assert!(system_volume_root(None).is_err());
        let here = std::env::current_dir().unwrap();
        let root = system_volume_root(Some(here.as_os_str())).unwrap();
        assert!(here.starts_with(&root) && root.parent().is_none());
    }
}
