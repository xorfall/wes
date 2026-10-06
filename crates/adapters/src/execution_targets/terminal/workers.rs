//! Own blocking pipe workers independently of output EOF. A supervisor keeps ownership
//! after a refused bounded join, and joins each thread only after its actual completion.
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
struct Failure {
    kind: io::ErrorKind,
    role: &'static str,
}
enum Message {
    Added(usize, &'static str, JoinHandle<io::Result<()>>),
    Finished(usize),
}
struct Completion(usize, mpsc::SyncSender<Message>);
impl Drop for Completion {
    fn drop(&mut self) {
        let _ = self.1.send(Message::Finished(self.0));
    }
}
pub(super) struct Workers {
    sender: Option<mpsc::SyncSender<Message>>,
    supervisor: Option<JoinHandle<()>>,
    failure: Arc<Mutex<Option<Failure>>>,
    pub(super) closing: Arc<AtomicBool>,
    started: usize,
}
impl Workers {
    pub(super) fn new() -> io::Result<Self> {
        // At most three Added and three Finished messages exist in one endpoint's lifetime.
        let (sender, receiver) = mpsc::sync_channel(6);
        let failure = Arc::new(Mutex::new(None));
        let recorded = failure.clone();
        let supervisor = thread::Builder::new()
            .name("wes-conpty-workers".into())
            .spawn(move || {
                let mut workers: Vec<(usize, &'static str, JoinHandle<io::Result<()>>)> = vec![];
                let mut notified = [false; 3];
                let mut joined = [false; 3];
                let mut disconnected = false;
                loop {
                    let mut i = 0;
                    while i < workers.len() {
                        if !workers[i].2.is_finished() {
                            i += 1;
                            continue;
                        }
                        let (id, role, worker) = workers.swap_remove(i);
                        let result = worker
                            .join()
                            .unwrap_or_else(|_| Err(io::ErrorKind::Other.into()));
                        if let Err(error) = result {
                            let mut first = recorded.lock().unwrap_or_else(|e| e.into_inner());
                            first.get_or_insert(Failure {
                                kind: error.kind(),
                                role,
                            });
                        }
                        joined[id] = true;
                    }
                    if disconnected && workers.is_empty() {
                        return;
                    }
                    if disconnected {
                        // The queue has no remaining producer. Avoid a busy loop if a thread's
                        // final teardown is still pending after its completion notification.
                        thread::sleep(Duration::from_millis(1));
                    }
                    let next = if disconnected || (0..3).any(|id| notified[id] && !joined[id]) {
                        receiver.recv_timeout(Duration::from_millis(1))
                    } else {
                        receiver
                            .recv()
                            .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                    };
                    match next {
                        Ok(Message::Added(id, role, worker)) => workers.push((id, role, worker)),
                        Ok(Message::Finished(id)) => notified[id] = true,
                        Err(mpsc::RecvTimeoutError::Disconnected) => disconnected = true,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            supervisor: Some(supervisor),
            failure,
            closing: Arc::new(AtomicBool::new(false)),
            started: 0,
        })
    }
    pub(super) fn spawn(
        &mut self,
        role: &'static str,
        job: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> io::Result<()> {
        if self.started == 3 {
            return Err(io::Error::other("Terminal worker capacity exhausted"));
        }
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| io::Error::other("Terminal workers are closing"))?;
        let completion = Completion(self.started, sender.clone());
        let worker = thread::Builder::new()
            .name(format!("wes-conpty-{role}"))
            .spawn(move || {
                let _completion = completion;
                job()
            })?;
        sender
            .send(Message::Added(self.started, role, worker))
            .map_err(|_| io::Error::other("Terminal worker supervisor ended"))?;
        self.started += 1;
        Ok(())
    }
    pub(super) fn begin_close(&self) {
        self.closing.store(true, Ordering::Release);
    }
    pub(super) fn check(&self) -> io::Result<()> {
        match *self.failure.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(failure) => Err(io::Error::new(
                failure.kind,
                format!("Terminal {} worker failed", failure.role),
            )),
            None => Ok(()),
        }
    }
    pub(super) fn finish(&mut self, deadline: Instant) -> io::Result<()> {
        self.begin_close();
        self.sender.take();
        if let Some(supervisor) = &self.supervisor {
            while !supervisor.is_finished() {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Terminal pipe cleanup could not be confirmed",
                    ));
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
        if let Some(supervisor) = self.supervisor.take() {
            supervisor
                .join()
                .map_err(|_| io::Error::other("Terminal worker supervisor failed"))?;
        }
        self.check()
    }
}
impl Drop for Workers {
    fn drop(&mut self) {
        // The supervisor retains and eventually joins unfinished workers. A timed-out finish
        // has already refused a cleanup receipt; dropping the endpoint cannot change that.
        self.sender.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_completion_does_not_join_a_blocked_input_worker() {
        let mut workers = Workers::new().unwrap();
        let (release, blocked) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        let retained = Arc::new(());
        let owned = retained.clone();
        workers
            .spawn("input", move || {
                let _owned = owned;
                ready.send(()).unwrap();
                blocked.recv().unwrap();
                Ok(())
            })
            .unwrap();
        workers.spawn("output", || Ok(())).unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        let error = workers.finish(Instant::now()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(Arc::strong_count(&retained), 2);
        release.send(()).unwrap();
        workers
            .finish(Instant::now() + Duration::from_secs(2))
            .unwrap();
        assert_eq!(Arc::strong_count(&retained), 1);
    }
    #[test]
    fn worker_failures_and_panics_are_not_clean_completion() {
        for panic in [false, true] {
            let mut workers = Workers::new().unwrap();
            workers
                .spawn("input", move || {
                    if panic {
                        panic!("synthetic worker failure");
                    }
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "synthetic private details",
                    ))
                })
                .unwrap();
            let error = workers
                .finish(Instant::now() + Duration::from_secs(2))
                .unwrap_err();
            assert_eq!(
                error.kind(),
                if panic {
                    io::ErrorKind::Other
                } else {
                    io::ErrorKind::PermissionDenied
                }
            );
            assert_eq!(error.to_string(), "Terminal input worker failed");
        }
    }
    #[test]
    fn dropping_a_timed_out_endpoint_retains_the_worker_owner_until_completion() {
        let mut workers = Workers::new().unwrap();
        let supervisor = workers.supervisor.take().unwrap();
        let (release, blocked) = mpsc::channel();
        workers
            .spawn("close", move || {
                blocked.recv().unwrap();
                Ok(())
            })
            .unwrap();
        drop(workers);
        assert!(!supervisor.is_finished());
        release.send(()).unwrap();
        supervisor.join().unwrap();
    }
}
