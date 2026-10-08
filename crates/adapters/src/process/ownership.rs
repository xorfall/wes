//! An invocation owns its local execution group until it completes or is cancelled.
//! Normal success releases ownership without ending intentional background effects.
use process_wrap::tokio::{ChildWrapper, CommandWrap};
use std::{any::Any, io};
use tokio::process::{Child, Command};

#[derive(Debug)]
pub(crate) struct CleanupError {
    phase: &'static str,
    source: io::Error,
}
impl CleanupError {
    pub(super) fn summary(&self) -> String {
        // Only host error classification is public; never copy arbitrary I/O error text.
        match self.source.raw_os_error() {
            Some(code) => format!("{}: {:?} (OS error {code})", self.phase, self.source.kind()),
            None => format!("{}: {:?}", self.phase, self.source.kind()),
        }
    }
}
fn confirm_cleanup(
    signalled: io::Result<()>,
    reaped: io::Result<()>,
    group_probe: impl FnOnce() -> io::Result<()>,
) -> Result<(), CleanupError> {
    reaped.map_err(|source| CleanupError {
        phase: "wait",
        source,
    })?;
    // An already-empty Unix group needs no signal. Other refusals cannot claim cleanup.
    #[cfg(unix)]
    if let Err(error) = &signalled {
        if error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) {
            return Ok(());
        }
        // macOS can report EPERM for a group containing only an exited, unreaped
        // leader. A successful wait alone says nothing about surviving descendants.
        // Resolve that race only when a fresh, non-signalling probe proves the group
        // is now absent. A live group, another refusal or an unreadable group fails.
        if error.kind() == io::ErrorKind::PermissionDenied
            && group_probe().is_err_and(|probe| {
                probe.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error())
            })
        {
            return Ok(());
        }
    }
    #[cfg(not(unix))]
    let _ = group_probe;
    signalled.map_err(|source| CleanupError {
        phase: "signal",
        source,
    })
}

pub(super) struct LocalChild {
    child: Box<dyn ChildWrapper>,
    completed: bool,
    #[cfg(unix)]
    group: Option<rustix::process::Pid>,
}
impl LocalChild {
    pub(super) fn spawn(command: Command, inherited_terminal: bool) -> io::Result<Self> {
        let mut wrapped = CommandWrap::from(command);
        // A foreground terminal handover borrows its host's job-control group. Creating a
        // different group there would stop reads with SIGTTIN; its host keeps terminal ownership.
        #[cfg(unix)]
        if !inherited_terminal {
            wrapped.wrap(process_wrap::tokio::ProcessGroup::leader());
        }
        #[cfg(windows)]
        {
            let _ = inherited_terminal;
            // The wrapper starts suspended, assigns the Job Object, then resumes: a child
            // cannot spawn an escaping descendant in a spawn/assignment window.
            wrapped.wrap(process_wrap::tokio::JobObject);
        }
        let child = wrapped.spawn()?;
        #[cfg(unix)]
        let group = if inherited_terminal {
            None
        } else {
            child
                .id()
                .and_then(|id| rustix::process::Pid::from_raw(id as i32))
        };
        Ok(Self {
            child,
            completed: false,
            #[cfg(unix)]
            group,
        })
    }
    pub(super) fn inner(&mut self) -> &mut Child {
        let mut child = self.child.as_mut();
        loop {
            if (&*child as &dyn Any).is::<Child>() {
                return (child as &mut dyn Any).downcast_mut::<Child>().unwrap();
            }
            child = child.inner_mut();
        }
    }
    pub(super) fn complete(&mut self) {
        // No KillOnDrop wrapper: successful background effects keep their previous lifetime.
        self.completed = true;
    }
    pub(super) async fn cancel(&mut self) -> Result<(), CleanupError> {
        let signalled = self.child.start_kill();
        let reaped = self.child.wait().await.map(|_| ());
        confirm_cleanup(signalled, reaped, || {
            #[cfg(unix)]
            return match self.group {
                Some(group) => rustix::process::test_kill_process_group(group).map_err(Into::into),
                // A terminal handover does not own its host's process group.
                None => Err(io::ErrorKind::PermissionDenied.into()),
            };
            #[cfg(not(unix))]
            unreachable!("group probing is Unix-only")
        })?;
        self.completed = true;
        Ok(())
    }
}
impl Drop for LocalChild {
    fn drop(&mut self) {
        if !self.completed {
            let _ = self.child.start_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, path::Path, process::Stdio, time::Duration};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[tokio::test]
    async fn completing_the_owner_keeps_intentional_background_effects_alive() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join(if cfg!(windows) {
            "fixture.exe"
        } else {
            "fixture"
        });
        let rustc = Path::new(env!("CARGO")).with_file_name(if cfg!(windows) {
            "rustc.exe"
        } else {
            "rustc"
        });
        let built = std::process::Command::new(rustc)
            .arg("--edition=2024")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/process_fixture.rs"))
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let lock = root.path().join("parent.lock");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut command = Command::new(executable);
        command
            .arg("background")
            .arg(&lock)
            .arg(listener.local_addr().unwrap().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let mut child = LocalChild::spawn(command, false).unwrap();
        let (mut control, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        control.read_exact(&mut [0; 5]).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_secs(2), child.inner().wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
        child.complete();
        drop(child);
        // The exact owner used by invocation has dropped; the descendant is still live.
        assert!(
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(lock.with_extension("desc.lock"))
                .unwrap()
                .try_lock()
                .is_err()
        );
        control.write_all(b"x").await.unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(1), control.read(&mut [0; 1]))
            .await
            .unwrap();
        assert!(
            matches!(ended, Ok(0))
                || matches!(ended, Err(ref error) if error.kind() == io::ErrorKind::ConnectionReset)
        );
    }
}

#[cfg(test)]
mod cleanup_tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn a_refused_signal_is_resolved_only_by_wait_and_confirmed_group_absence() {
        let denied = || Err(io::ErrorKind::PermissionDenied.into());
        let absent = || {
            Err(io::Error::from_raw_os_error(
                rustix::io::Errno::SRCH.raw_os_error(),
            ))
        };
        assert!(confirm_cleanup(denied(), Ok(()), absent).is_ok());
        for probe in [Ok(()), denied(), Err(io::ErrorKind::Interrupted.into())] {
            let error = confirm_cleanup(denied(), Ok(()), || probe).unwrap_err();
            assert_eq!(error.summary(), "signal: PermissionDenied");
        }
        assert_eq!(
            confirm_cleanup(denied(), denied(), || panic!("failed wait must not probe"))
                .unwrap_err()
                .summary(),
            "wait: PermissionDenied"
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn cancelling_an_exited_unreaped_group_leader_confirms_cleanup() {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let mut command = Command::new("/usr/bin/true");
        command.kill_on_drop(true);
        let mut child = LocalChild::spawn(command, false).unwrap();
        let pid = Pid::from_raw(child.inner().id().unwrap() as i32).unwrap();
        // Observe exit without reaping. The cancellation now starts with a zombie
        // group leader, independently of scheduler timing or pipe buffering.
        waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        )
        .unwrap();
        child.cancel().await.unwrap();
        assert_eq!(
            rustix::process::test_kill_process_group(pid),
            Err(rustix::io::Errno::SRCH)
        );
    }

    #[test]
    fn waiting_and_signalling_refusals_keep_their_phase_without_private_error_text() {
        let error = confirm_cleanup(
            Ok(()),
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "synthetic private text",
            )),
            || panic!("failed wait must not probe"),
        )
        .unwrap_err();
        assert_eq!(error.summary(), "wait: Interrupted");
        let error = confirm_cleanup(Err(io::ErrorKind::PermissionDenied.into()), Ok(()), || {
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.summary(), "signal: PermissionDenied");
        assert!(
            confirm_cleanup(Ok(()), Ok(()), || panic!(
                "successful signal needs no probe"
            ))
            .is_ok()
        );
    }
    #[cfg(unix)]
    #[test]
    fn an_empty_group_requires_a_confirmed_wait() {
        let absent = || {
            Err(io::Error::from_raw_os_error(
                rustix::io::Errno::SRCH.raw_os_error(),
            ))
        };
        assert!(
            confirm_cleanup(absent(), Ok(()), || panic!("absent signal needs no probe")).is_ok()
        );
        assert_eq!(
            confirm_cleanup(
                absent(),
                Err(io::ErrorKind::PermissionDenied.into()),
                || panic!("failed wait must not probe")
            )
            .unwrap_err()
            .summary(),
            "wait: PermissionDenied"
        );
    }
}
