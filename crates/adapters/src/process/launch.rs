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
