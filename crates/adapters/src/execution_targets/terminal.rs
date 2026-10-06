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
/// Windows matches environment names without regard to case.
pub(super) fn reserved_variable(key: &str) -> bool {
    let key = if cfg!(windows) {
        key.to_ascii_uppercase()
    } else {
        key.to_owned()
    };
    key.starts_with("WES_") || matches!(key.as_str(), "ZDOTDIR" | "ENV" | "BASH_ENV")
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
                    if reserved_variable(key) {
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

/// The console's startup request for the cursor position, found in a byte stream whose reads
/// are not frame boundaries. Only the request itself is removed; every other byte is passed on
/// in order. A trailing fragment that could still begin the request is held back until the
/// next read decides it, and the search ends after a bounded amount of startup output.
#[cfg(any(windows, test))]
#[derive(Default)]
struct CursorRequest {
    held: Vec<u8>,
    inspected: usize,
    decided: bool,
}
#[cfg(any(windows, test))]
impl CursorRequest {
    const REQUEST: &'static [u8] = b"\x1b[6n";
    const WINDOW: usize = 16 * 1024;

    /// The bytes to pass on from this read, and whether the request was removed from it.
    fn filter(&mut self, read: &[u8]) -> (Vec<u8>, bool) {
        if self.decided {
            return (read.to_vec(), false);
        }
        let mut bytes = std::mem::take(&mut self.held);
        bytes.extend_from_slice(read);
        let found = bytes
            .windows(Self::REQUEST.len())
            .position(|window| window == Self::REQUEST);
        if let Some(start) = found {
            bytes.drain(start..start + Self::REQUEST.len());
            self.decided = true;
            return (bytes, true);
        }
        let undecided = (1..Self::REQUEST.len())
            .rev()
            .find(|length| bytes.ends_with(&Self::REQUEST[..*length]))
            .unwrap_or(0);
        self.inspected += bytes.len() - undecided;
        if self.inspected > Self::WINDOW {
            self.decided = true;
            return (bytes, false);
        }
        self.held = bytes.split_off(bytes.len() - undecided);
        (bytes, false)
    }
    /// What was still held back when the stream ended.
    fn finish(&mut self) -> Vec<u8> {
        self.decided = true;
        std::mem::take(&mut self.held)
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
#[cfg(any(windows, test))]
mod workers;

#[cfg(windows)]
mod pty {
    //! ConPTY offers only blocking pipes. One thread per direction keeps the shared terminal
    //! loop from stalling on either; both queues are bounded, so a stalled side applies
    //! backpressure instead of growing memory.
    use super::*;
    use portable_pty::{Child, CommandBuilder, ExitStatus, MasterPty, PtySize};
    use std::{
        io::{Read, Write},
        sync::atomic::Ordering,
        sync::mpsc,
        time::Instant,
    };

    const CHUNK: usize = 16 * 1024;
    const OUTPUT_CHUNKS: usize = 64;
    const INPUT_CHUNKS: usize = 16;
    /// A closed console asks its processes to leave before the shell is terminated.
    const CLOSE_GRACE: Duration = Duration::from_secs(3);
    const DRAIN: Duration = Duration::from_secs(2);

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
        let pair = portable_pty::native_pty_system()
            .openpty(dimensions(size))
            .map_err(io::Error::other)?;
        let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let writer = pair.master.take_writer().map_err(io::Error::other)?;
        let mut command = CommandBuilder::new(launch.executable);
        command.args(launch.arguments);
        command.env_clear();
        if let Some(cwd) = launch.cwd {
            command.cwd(cwd);
        }
        for (key, value) in launch.environment {
            command.env(key, value);
        }
        // Both threads end on their own if the launch below fails: dropping the pair closes
        // the console and the input queue.
        let (input, queued) = mpsc::sync_channel::<Vec<u8>>(INPUT_CHUNKS);
        let (produced, output) = mpsc::sync_channel::<Vec<u8>>(OUTPUT_CHUNKS);
        let reply = input.clone();
        let mut workers = workers::Workers::new()?;
        let closing = workers.closing.clone();
        workers.spawn("output", move || read_output(reader, produced, reply))?;
        workers.spawn("input", move || write_input(writer, queued, closing))?;
        if cancellation.is_cancelled() {
            return Err(io::Error::other("Terminal launch authority ended"));
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(io::Error::other)?;
        drop(pair.slave);
        Ok(Box::new(Pty {
            master: Some(pair.master),
            child,
            output,
            pending: vec![],
            cursor: 0,
            eof: false,
            input: Some(input),
            workers,
            cleanup_attempted: false,
            remote,
            exited: None,
            ended: None,
        }))
    }
    /// The console asks where the cursor is before it starts and waits for the answer. A new
    /// pane always starts at the origin, so the request is answered here and never displayed;
    /// an emulator answering it as well would deliver a stray key to the shell.
    fn read_output(
        mut reader: Box<dyn Read + Send>,
        produced: mpsc::SyncSender<Vec<u8>>,
        reply: mpsc::SyncSender<Vec<u8>>,
    ) -> io::Result<()> {
        let mut reply = Some(reply);
        let mut request = CursorRequest::default();
        let mut buffer = vec![0u8; CHUNK];
        loop {
            let n = match reader.read(&mut buffer) {
                Ok(0) => break,
                Err(error) if pipe_closed(&error) => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
                Ok(n) => n,
            };
            let (chunk, found) = request.filter(&buffer[..n]);
            if found && let Some(reply) = reply.take() {
                let _ = reply.send(b"\x1b[1;1R".to_vec());
            }
            if request.decided {
                // The input queue must not be kept open by this thread once it cannot answer.
                reply = None;
            }
            if !chunk.is_empty() {
                // Even after the receiver leaves, drain the pipe so closing the console can finish.
                let _ = produced.send(chunk);
            }
        }
        let rest = request.finish();
        if !rest.is_empty() {
            let _ = produced.send(rest);
        }
        Ok(())
    }
    fn pipe_closed(error: &io::Error) -> bool {
        matches!(error.raw_os_error(), Some(109 | 232))
    }
    fn write_input(
        mut writer: Box<dyn Write + Send>,
        queued: mpsc::Receiver<Vec<u8>>,
        closing: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> io::Result<()> {
        while let Ok(bytes) = queued.recv() {
            if let Err(error) = writer.write_all(&bytes) {
                if closing.load(Ordering::Acquire) && pipe_closed(&error) {
                    return Ok(());
                }
                return Err(error);
            }
        }
        Ok(())
    }
    #[cfg(test)]
    mod io_tests {
        use super::*;
        struct Failed;
        impl Read for Failed {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::ErrorKind::PermissionDenied.into())
            }
        }
        impl Write for Failed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from_raw_os_error(109))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        #[test]
        fn an_unexpected_read_failure_is_not_eof() {
            let (produced, _) = mpsc::sync_channel(OUTPUT_CHUNKS);
            let (reply, _) = mpsc::sync_channel(INPUT_CHUNKS);
            assert_eq!(
                read_output(Box::new(Failed), produced, reply)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::PermissionDenied
            );
            assert!(pipe_closed(&io::Error::from_raw_os_error(109)));
            assert!(pipe_closed(&io::Error::from_raw_os_error(232)));
            assert!(!pipe_closed(&io::Error::from_raw_os_error(5)));
        }
        #[test]
        fn input_pipe_closure_is_normal_only_during_endpoint_close() {
            for closing in [false, true] {
                let (input, queued) = mpsc::sync_channel(INPUT_CHUNKS);
                input.send(vec![1]).unwrap();
                drop(input);
                let result = write_input(
                    Box::new(Failed),
                    queued,
                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(closing)),
                );
                assert_eq!(result.is_ok(), closing);
            }
        }
    }
    struct Pty {
        master: Option<Box<dyn MasterPty + Send>>,
        child: Box<dyn Child + Send + Sync>,
        output: mpsc::Receiver<Vec<u8>>,
        pending: Vec<u8>,
        cursor: usize,
        eof: bool,
        input: Option<mpsc::SyncSender<Vec<u8>>>,
        workers: workers::Workers,
        cleanup_attempted: bool,
        remote: bool,
        /// The shell has exited; its last output may still be on its way.
        exited: Option<(ExitStatus, Instant)>,
        ended: Option<TerminalExit>,
    }
    impl Pty {
        fn exit(&self, status: &ExitStatus, forced: bool) -> TerminalExit {
            TerminalExit {
                code: status.exit_code(),
                uncertain: self.remote
                    && (forced || status.exit_code() == 255 || status.signal().is_some()),
            }
        }
        fn drained(&self) -> bool {
            self.eof && self.cursor == self.pending.len()
        }
        /// Closing the console ends every process still attached to it and makes the output
        /// pipe report its end. Older systems do not return from the close until that pipe is
        /// read, so it runs beside the reader instead of in front of it.
        fn close_console(&mut self) -> io::Result<()> {
            self.workers.begin_close();
            self.input.take();
            if let Some(master) = self.master.take() {
                self.workers.spawn("close", move || {
                    drop(master);
                    Ok(())
                })?;
            }
            Ok(())
        }
        fn discard_until(&mut self, deadline: Instant) {
            self.pending.clear();
            self.cursor = 0;
            while !self.eof {
                let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                    break;
                };
                match self.output.recv_timeout(left) {
                    Ok(_) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => self.eof = true,
                }
            }
        }
    }
    impl Read for Pty {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.workers.check()?;
            if bytes.is_empty() {
                return Ok(0);
            }
            if self.cursor == self.pending.len() {
                if self.eof {
                    return Ok(0);
                }
                match self.output.try_recv() {
                    Ok(chunk) => {
                        self.pending = chunk;
                        self.cursor = 0;
                    }
                    Err(mpsc::TryRecvError::Empty) => return Err(io::ErrorKind::WouldBlock.into()),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.eof = true;
                        return Ok(0);
                    }
                }
            }
            let n = bytes.len().min(self.pending.len() - self.cursor);
            bytes[..n].copy_from_slice(&self.pending[self.cursor..self.cursor + n]);
            self.cursor += n;
            Ok(n)
        }
    }
    impl Write for Pty {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.workers.check()?;
            let input = self
                .input
                .as_ref()
                .ok_or_else(|| io::Error::other("Terminal is closed"))?;
            let n = bytes.len().min(CHUNK);
            match input.try_send(bytes[..n].to_vec()) {
                Ok(()) => Ok(n),
                Err(mpsc::TrySendError::Full(_)) => Err(io::ErrorKind::WouldBlock.into()),
                Err(mpsc::TrySendError::Disconnected(_)) => Err(io::ErrorKind::BrokenPipe.into()),
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl TerminalIo for Pty {
        fn resize(&mut self, size: TerminalSize) -> io::Result<()> {
            match &self.master {
                Some(master) => master.resize(dimensions(size)).map_err(io::Error::other),
                // The shell has exited and only its remaining output is being delivered.
                None => Ok(()),
            }
        }
        fn wait_ready(&mut self, _: bool, timeout: Duration) -> io::Result<()> {
            self.workers.check()?;
            if self.cursor < self.pending.len() {
                return Ok(());
            }
            if self.eof {
                std::thread::sleep(timeout);
                return Ok(());
            }
            match self.output.recv_timeout(timeout) {
                Ok(chunk) => {
                    self.pending = chunk;
                    self.cursor = 0;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => self.eof = true,
            }
            Ok(())
        }
        /// The exit is reported only once the output written before it has been read.
        fn try_exit(&mut self) -> io::Result<Option<TerminalExit>> {
            self.workers.check()?;
            if self.ended.is_some() {
                return Ok(self.ended);
            }
            if self.exited.is_none()
                && let Some(status) = self.child.try_wait()?
            {
                self.exited = Some((status, Instant::now() + DRAIN));
                self.close_console()?;
            }
            if let Some((status, deadline)) = &self.exited
                && (self.drained() || Instant::now() >= *deadline)
            {
                self.ended = Some(self.exit(status, false));
            }
            Ok(self.ended)
        }
        fn shutdown(&mut self) -> io::Result<TerminalExit> {
            if self.ended.is_none() {
                let (status, forced) = match self.exited.take() {
                    Some((status, _)) => (status, false),
                    None => match self.child.try_wait()? {
                        Some(status) => (status, false),
                        None => {
                            self.close_console()?;
                            let deadline = Instant::now() + CLOSE_GRACE;
                            loop {
                                self.discard_until(Instant::now() + Duration::from_millis(20));
                                if let Some(status) = self.child.try_wait()? {
                                    break (status, true);
                                }
                                if self.eof {
                                    std::thread::sleep(Duration::from_millis(10));
                                }
                                if Instant::now() >= deadline {
                                    let killed = self.child.kill();
                                    let status = self.child.wait()?;
                                    // A concurrent exit can make the kill fail; the join decides.
                                    let _ = killed;
                                    break (status, true);
                                }
                            }
                        }
                    },
                };
                self.ended = Some(self.exit(&status, forced));
            }
            self.close_console()?;
            self.cleanup_attempted = true;
            let deadline = Instant::now() + DRAIN;
            self.discard_until(deadline);
            self.workers.finish(deadline)?;
            Ok(self.ended.expect("recorded above"))
        }
    }
    impl Drop for Pty {
        fn drop(&mut self) {
            if !self.cleanup_attempted {
                let _ = self.shutdown();
            }
        }
    }
}
#[cfg(not(any(unix, windows)))]
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

#[cfg(test)]
mod cursor_request_tests {
    use super::CursorRequest;

    /// Feeds the stream in the given pieces; returns what was passed on and the replies due.
    fn run(pieces: &[&[u8]]) -> (Vec<u8>, usize) {
        let mut request = CursorRequest::default();
        let (mut shown, mut replies) = (vec![], 0);
        for piece in pieces {
            let (bytes, found) = request.filter(piece);
            shown.extend(bytes);
            replies += usize::from(found);
        }
        shown.extend(request.finish());
        (shown, replies)
    }
    fn every_split(stream: &[u8]) -> Vec<Vec<&[u8]>> {
        let mut splits = vec![vec![stream]];
        for first in 0..=stream.len() {
            splits.push(vec![&stream[..first], &stream[first..]]);
            for second in first..=stream.len() {
                splits.push(vec![
                    &stream[..first],
                    &stream[first..second],
                    &stream[second..],
                ]);
            }
        }
        splits.push(stream.chunks(1).collect());
        splits
    }

    #[test]
    fn the_request_is_removed_once_wherever_the_reads_divide_it() {
        let stream = b"\x1b[?9001h\x1b[?1004h\x1b[6n\x1b[2J\x1b[Hprompt> ";
        for pieces in every_split(stream) {
            let (shown, replies) = run(&pieces);
            assert_eq!(
                shown, b"\x1b[?9001h\x1b[?1004h\x1b[2J\x1b[Hprompt> ",
                "{pieces:?}"
            );
            assert_eq!(replies, 1, "{pieces:?}");
        }
        // The case from review: the request arrives as its first two bytes, then the rest.
        assert_eq!(run(&[b"\x1b[", b"6n"]), (vec![], 1));
    }

    #[test]
    fn other_sequences_pass_in_order_and_only_the_first_request_is_answered() {
        for stream in [
            &b"\x1b[31mred\x1b[0m \x1b[6 n \x1b[6\x1b[7n \x1b\x1b[ 6n tail\x1b["[..],
            b"\x1b",
            b"\x1b[6",
            b"",
        ] {
            for pieces in every_split(stream) {
                assert_eq!(run(&pieces), (stream.to_vec(), 0), "{pieces:?}");
            }
        }
        // An escape directly before the request is unrelated output and is kept.
        for pieces in every_split(b"\x1b\x1b[6n") {
            assert_eq!(run(&pieces), (b"\x1b".to_vec(), 1), "{pieces:?}");
        }
        // A later request belongs to the program in the pane and reaches the emulator.
        for pieces in every_split(b"a\x1b[6nb\x1b[6nc") {
            assert_eq!(run(&pieces), (b"ab\x1b[6nc".to_vec(), 1), "{pieces:?}");
        }
    }

    #[test]
    fn the_search_is_bounded_and_holds_back_at_most_a_fragment() {
        let mut request = CursorRequest::default();
        let (shown, found) = request.filter(b"text\x1b[6");
        assert_eq!((shown.as_slice(), found), (&b"text"[..], false));
        assert_eq!(request.held, b"\x1b[6");
        let (shown, found) = request.filter(b"m");
        assert_eq!((shown.as_slice(), found), (&b"\x1b[6m"[..], false));
        assert!(request.held.is_empty());

        // After the startup window a request is ordinary output: nothing is removed or held.
        let mut request = CursorRequest::default();
        let filler = vec![b'x'; CursorRequest::WINDOW + 1];
        assert_eq!(request.filter(&filler), (filler.clone(), false));
        assert!(request.decided);
        assert_eq!(request.filter(b"\x1b[6n"), (b"\x1b[6n".to_vec(), false));
        assert_eq!(request.filter(b"\x1b["), (b"\x1b[".to_vec(), false));
        assert!(request.finish().is_empty());
    }
}

#[cfg(all(test, windows))]
mod conpty_tests;
