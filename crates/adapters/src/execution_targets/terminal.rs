use std::{collections::BTreeMap, ffi::OsString, io, path::PathBuf, time::Duration};
use wes_core::environments::Target;
use wes_engine::{
    driver::CancellationToken,
    execution::{TerminalExit, TerminalIo, TerminalSize},
};

/// Host launch material stays inside adapters/application; never an engine wire command.
#[derive(Default)]
pub struct HostLaunch {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub environment: BTreeMap<OsString, OsString>,
}
impl HostLaunch {
    pub fn arg(&mut self, value: impl Into<OsString>) {
        self.arguments.push(value.into());
    }
    pub fn env(&mut self, key: impl Into<OsString>, value: impl Into<OsString>) {
        self.environment.insert(key.into(), value.into());
    }
}
#[derive(Clone, Copy)]
pub struct TerminalSupport {
    pub label: &'static str,
    pub workspace_tools: bool,
}
pub struct TerminalPlan {
    support: TerminalSupport,
    opener: Box<
        dyn FnOnce(
                Option<HostLaunch>,
                TerminalSize,
                &CancellationToken,
            ) -> io::Result<Box<dyn TerminalIo>>
            + Send,
    >,
}
impl TerminalPlan {
    /// Driver-owned opening strategy; a socket driver can supply an endpoint without a PTY.
    pub(crate) fn new(
        support: TerminalSupport,
        opener: impl FnOnce(
            Option<HostLaunch>,
            TerminalSize,
            &CancellationToken,
        ) -> io::Result<Box<dyn TerminalIo>>
        + Send
        + 'static,
    ) -> Self {
        Self {
            support,
            opener: Box::new(opener),
        }
    }

    pub fn workspace() -> Self {
        local(None)
    }
    pub(crate) fn with_preflight(
        self,
        check: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> Self {
        Self::new(self.support, move |host, size, cancellation| {
            check()?;
            (self.opener)(host, size, cancellation)
        })
    }
    pub fn for_target(target: &Target) -> Result<Self, &'static str> {
        super::driver(target).terminal(target)
    }
    pub fn support(&self) -> TerminalSupport {
        self.support
    }
    pub(crate) fn remote(launch: HostLaunch, label: &'static str) -> Self {
        Self::new(
            TerminalSupport {
                label,
                workspace_tools: false,
            },
            move |host, size, cancellation| {
                if host.is_some() {
                    return Err(io::Error::other(
                        "Remote terminals cannot receive workspace bootstrap material",
                    ));
                }
                pty::open(launch, size, true, cancellation)
            },
        )
    }
    pub fn open(
        self,
        host: Option<HostLaunch>,
        size: TerminalSize,
        cancellation: &CancellationToken,
    ) -> io::Result<Box<dyn TerminalIo>> {
        if cancellation.is_cancelled() {
            return Err(io::Error::other("Terminal launch authority ended"));
        }
        (self.opener)(host, size, cancellation)
    }
}
pub(super) fn local(target: Option<Target>) -> TerminalPlan {
    TerminalPlan {
        support: TerminalSupport {
            label: "Local",
            workspace_tools: true,
        },
        opener: Box::new(move |host, size, cancellation| {
            let mut host = host.ok_or_else(|| {
                io::Error::other("Local terminal requires host shell preparation")
            })?;
            if let Some(target) = target {
                if let Some(cwd) = target.cwd() {
                    host.cwd = Some(cwd.into());
                }
                // Application integration variables are reserved by the host bootstrap.
                for (key, value) in target.variables() {
                    if key.starts_with("WES_")
                        || matches!(key.as_str(), "ZDOTDIR" | "ENV" | "BASH_ENV")
                    {
                        return Err(io::Error::other(
                            "Target variables cannot replace terminal integration/startup controls",
                        ));
                    }
                    host.env(key, value);
                }
            }
            pty::open(host, size, false, cancellation)
        }),
    }
}

#[cfg(unix)]
mod pty {
    use super::*;
    use portable_pty::{Child, CommandBuilder, MasterPty, PtySize};
    use std::io::{Read, Write};
    fn dimensions(size: TerminalSize) -> PtySize {
        PtySize {
            cols: size.cols,
            rows: size.rows,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
    pub(super) fn open(
        launch: HostLaunch,
        size: TerminalSize,
        remote: bool,
        cancellation: &CancellationToken,
    ) -> io::Result<Box<dyn TerminalIo>> {
        crate::process::serialized_spawn(|| open_owned(launch, size, remote, cancellation))
    }
    fn open_owned(
        launch: HostLaunch,
        size: TerminalSize,
        remote: bool,
        cancellation: &CancellationToken,
    ) -> io::Result<Box<dyn TerminalIo>> {
        let pair = portable_pty::native_pty_system()
            .openpty(dimensions(size))
            .map_err(io::Error::other)?;
        let raw = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| io::Error::other("PTY has no Unix descriptor"))?;
        let mut fd = filedescriptor::FileDescriptor::dup(&raw).map_err(io::Error::other)?;
        fd.set_non_blocking(true).map_err(io::Error::other)?;
        let mut command = CommandBuilder::new(launch.executable);
        command.args(launch.arguments);
        command.env_clear();
        if let Some(cwd) = launch.cwd {
            command.cwd(cwd);
        }
        for (key, value) in launch.environment {
            command.env(key, value);
        }
        if cancellation.is_cancelled() {
            return Err(io::Error::other("Terminal launch authority ended"));
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(io::Error::other)?;
        drop(pair.slave);
        Ok(Box::new(Pty {
            fd: Some(fd),
            master: Some(pair.master),
            child,
            remote,
            ended: None,
        }))
    }
    struct Pty {
        fd: Option<filedescriptor::FileDescriptor>,
        master: Option<Box<dyn MasterPty + Send>>,
        child: Box<dyn Child + Send + Sync>,
        remote: bool,
        ended: Option<TerminalExit>,
    }
    impl Pty {
        fn exit(&self, status: portable_pty::ExitStatus, forced: bool) -> TerminalExit {
            TerminalExit {
                code: status.exit_code(),
                uncertain: self.remote
                    && (forced || status.exit_code() == 255 || status.signal().is_some()),
            }
        }
    }
    impl Read for Pty {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            match self
                .fd
                .as_mut()
                .ok_or_else(|| io::Error::other("Terminal is closed"))?
                .read(bytes)
            {
                Err(e) if e.raw_os_error() == Some(5) => Ok(0),
                other => other,
            }
        }
    }
    impl Write for Pty {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.fd
                .as_mut()
                .ok_or_else(|| io::Error::other("Terminal is closed"))?
                .write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl TerminalIo for Pty {
        fn resize(&mut self, size: TerminalSize) -> io::Result<()> {
            self.master
                .as_ref()
                .ok_or_else(|| io::Error::other("Terminal is closed"))?
                .resize(dimensions(size))
                .map_err(io::Error::other)
        }
        fn wait_ready(&mut self, writing: bool, timeout: Duration) -> io::Result<()> {
            let raw = self
                .master
                .as_ref()
                .and_then(|master| master.as_raw_fd())
                .ok_or_else(|| io::Error::other("Terminal is closed"))?;
            let mut events = [filedescriptor::pollfd {
                fd: raw,
                events: filedescriptor::POLLIN | if writing { filedescriptor::POLLOUT } else { 0 },
                revents: 0,
            }];
            if filedescriptor::poll(&mut events, Some(timeout)).is_err() {
                std::thread::sleep(timeout);
            }
            Ok(())
        }
        fn try_exit(&mut self) -> io::Result<Option<TerminalExit>> {
            if let Some(exit) = self.ended {
                return Ok(Some(exit));
            }
            if let Some(status) = self.child.try_wait()? {
                self.ended = Some(self.exit(status, false));
            }
            Ok(self.ended)
        }
        fn shutdown(&mut self) -> io::Result<TerminalExit> {
            if let Some(exit) = self.try_exit()? {
                self.fd.take();
                self.master.take();
                return Ok(exit);
            }
            self.fd.take();
            self.master.take();
            let killed = self.child.kill();
            let status = self.child.wait()?;
            self.ended = Some(self.exit(status, true));
            // A concurrently exited process can make kill fail; a successful join is authoritative.
            let _ = killed;
            Ok(self.ended.unwrap())
        }
    }
    impl Drop for Pty {
        fn drop(&mut self) {
            let _ = self.shutdown();
        }
    }
}
#[cfg(not(unix))]
mod pty {
    use super::*;
    pub(super) fn open(
        _: HostLaunch,
        _: TerminalSize,
        _: bool,
        _: &CancellationToken,
    ) -> io::Result<Box<dyn TerminalIo>> {
        Err(io::Error::other(
            "Terminal transport is not supported on this host platform",
        ))
    }
}
