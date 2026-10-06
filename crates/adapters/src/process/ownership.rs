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
fn confirm_cleanup(signalled: io::Result<()>, reaped: io::Result<()>) -> Result<(), CleanupError> {
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
    }
    signalled.map_err(|source| CleanupError {
        phase: "signal",
        source,
    })
}

pub(super) struct LocalChild {
    child: Box<dyn ChildWrapper>,
    completed: bool,
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
        Ok(Self {
            child: wrapped.spawn()?,
            completed: false,
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
        confirm_cleanup(signalled, reaped)?;
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
    #[test]
    fn waiting_and_signalling_refusals_keep_their_phase_without_private_error_text() {
        let error = confirm_cleanup(
            Ok(()),
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "synthetic private text",
            )),
        )
        .unwrap_err();
        assert_eq!(error.summary(), "wait: Interrupted");
        let error =
            confirm_cleanup(Err(io::ErrorKind::PermissionDenied.into()), Ok(())).unwrap_err();
        assert_eq!(error.summary(), "signal: PermissionDenied");
        assert!(confirm_cleanup(Ok(()), Ok(())).is_ok());
    }
    #[cfg(unix)]
    #[test]
    fn an_empty_group_requires_a_confirmed_wait() {
        let absent = || {
            Err(io::Error::from_raw_os_error(
                rustix::io::Errno::SRCH.raw_os_error(),
            ))
        };
        assert!(confirm_cleanup(absent(), Ok(())).is_ok());
        assert_eq!(
            confirm_cleanup(absent(), Err(io::ErrorKind::PermissionDenied.into()))
                .unwrap_err()
                .summary(),
            "wait: PermissionDenied"
        );
    }
}
