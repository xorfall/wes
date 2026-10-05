use indexmap::IndexMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;
use tokio::sync::oneshot;
use wes_engine::{
    driver::CancellationToken,
    type_sources::{TypeSourceCapture, TypeSourceError, TypeSourceReader, max_package_bytes},
    workspace::Workspace,
};
use wes_language::Span;

struct Reader<F>(F);
impl<F> TypeSourceReader for Reader<F>
where
    F: Fn(&str, usize) -> Result<String, TypeSourceError> + Send + Sync + 'static,
{
    fn read(&self, path: &str, max_bytes: usize) -> Result<String, TypeSourceError> {
        (self.0)(path, max_bytes)
    }
}
fn reader(
    f: impl Fn(&str, usize) -> Result<String, TypeSourceError> + Send + Sync + 'static,
) -> Arc<dyn TypeSourceReader> {
    Arc::new(Reader(f))
}
fn token() -> CancellationToken {
    CancellationToken::new()
}

#[tokio::test]
async fn successful_path_reads_are_stable_within_a_capture_but_not_across_submissions() {
    let calls = Arc::new(AtomicUsize::new(0));
    let reads = calls.clone();
    let reader =
        reader(move |_, _| Ok(format!("package-{}", reads.fetch_add(1, Ordering::SeqCst))));
    let mut capture = TypeSourceCapture::live(reader.clone());
    let first = capture.read("types.yaml", token()).await.unwrap();
    let same = capture.read("types.yaml", token()).await.unwrap();
    assert!(std::ptr::eq(first.source(), same.source()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    capture.accept(&same).unwrap();
    capture.accept(&first).unwrap();
    assert_eq!(
        capture.finish(),
        IndexMap::from([("types.yaml".to_string(), "package-0".to_string())])
    );
    let next = TypeSourceCapture::live(reader)
        .read("types.yaml", token())
        .await
        .unwrap();
    assert_eq!(next.source(), "package-1");
}

#[tokio::test]
async fn only_accepted_package_inputs_are_recorded_and_replay_never_needs_a_reader() {
    let mut capture = TypeSourceCapture::live(reader(|path, _| {
        Ok(match path {
            "good.yaml" => "types: {Positive: {base: Int, min: 1}}",
            "bad.yaml" => "types: {Broken: {base: Missing}}",
            _ => "[unclosed",
        }
        .into())
    }));
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let mut draft = workspace.draft().unwrap();
    for path in ["bad.yaml", "good.yaml", "syntax.yaml"] {
        let package = capture.read(path, token()).await.unwrap();
        if let Ok(change) = draft.prepare_type_package(package.source(), Span::at(0)) {
            draft.stage(change).unwrap();
            capture.accept(&package).unwrap();
        }
    }
    workspace
        .commit_batch(draft.finish(), std::time::Duration::ZERO)
        .unwrap();
    let captured = capture.finish();
    assert_eq!(
        captured.keys().map(String::as_str).collect::<Vec<_>>(),
        ["good.yaml"]
    );
    let mut replay = TypeSourceCapture::replay(captured).unwrap();
    let package = replay.read("good.yaml", token()).await.unwrap();
    let mut restored = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let prepared = restored
        .prepare_type_package(package.source(), Span::at(0))
        .unwrap();
    restored.commit(prepared).unwrap();
    assert!(restored.contracts().resolve("Positive").is_ok());
    assert!(restored.runtime().graph().is_empty());
    assert_eq!(
        replay.read("bad.yaml", token()).await.unwrap_err(),
        TypeSourceError::MissingSnapshot
    );
    assert_eq!(
        replay.read("new.yaml", token()).await.unwrap_err(),
        TypeSourceError::MissingSnapshot
    );
}

#[tokio::test]
async fn failed_reads_are_retryable_but_cannot_be_promoted() {
    let calls = Arc::new(AtomicUsize::new(0));
    let reads = calls.clone();
    let mut capture = TypeSourceCapture::live(reader(move |_, _| {
        if reads.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(TypeSourceError::Unavailable)
        } else {
            Ok("content".into())
        }
    }));
    assert_eq!(
        capture.read("test.yaml", token()).await.unwrap_err(),
        TypeSourceError::Unavailable
    );
    let package = capture.read("test.yaml", token()).await.unwrap();
    capture.accept(&package).unwrap();
    assert_eq!(capture.finish()["test.yaml"], "content");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn captured_packages_cannot_be_accepted_by_another_submission() {
    let mut first = TypeSourceCapture::live(reader(|_, _| Ok("private-source".into())));
    let package = first.read("private-path.yaml", token()).await.unwrap();
    let mut other = TypeSourceCapture::replay(IndexMap::from([(
        "private-path.yaml".into(),
        "private-source".into(),
    )]))
    .unwrap();
    assert_eq!(
        other.accept(&package).unwrap_err(),
        TypeSourceError::ForeignCapture
    );
    assert!(other.finish().is_empty());
    assert!(!format!("{package:?}").contains("private"));
    first.accept(&package).unwrap();
    assert_eq!(first.finish().len(), 1);
}

#[tokio::test]
async fn invalid_paths_are_rejected_before_reader_entry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let reads = calls.clone();
    let mut capture = TypeSourceCapture::live(reader(move |_, _| {
        reads.fetch_add(1, Ordering::SeqCst);
        Ok(String::new())
    }));
    for path in [
        "".into(),
        " \t".into(),
        "bad\0path".into(),
        "bad\npath".into(),
        "a".repeat(4097),
    ] {
        assert_eq!(
            capture.read(&path, token()).await.unwrap_err(),
            TypeSourceError::InvalidPath
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        capture
            .read("目录/types.yaml", token())
            .await
            .unwrap()
            .path(),
        "目录/types.yaml"
    );
}

#[tokio::test]
async fn count_limit_prevents_new_reads_and_keeps_existing_cache_accessible() {
    let calls = Arc::new(AtomicUsize::new(0));
    let reads = calls.clone();
    let mut capture = TypeSourceCapture::live(reader(move |_, _| {
        reads.fetch_add(1, Ordering::SeqCst);
        Ok(String::new())
    }));
    for index in 0..256 {
        capture
            .read(&format!("{index}.yaml"), token())
            .await
            .unwrap();
    }
    assert_eq!(
        capture.read("overflow.yaml", token()).await.unwrap_err(),
        TypeSourceError::Capacity
    );
    capture.read("0.yaml", token()).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 256);
    assert!(capture.finish().is_empty());
}

#[tokio::test]
async fn per_file_and_aggregate_budgets_are_checked_even_if_a_reader_violates_its_limit() {
    let mut capture = TypeSourceCapture::live(reader(|_, max| {
        assert_eq!(max, max_package_bytes());
        Ok("x".repeat(max + 1))
    }));
    assert_eq!(
        capture.read("big", token()).await.unwrap_err(),
        TypeSourceError::TooLarge
    );
    assert!(capture.finish().is_empty());
    let limits = Arc::new(Mutex::new(vec![]));
    let observed = limits.clone();
    let mut capture = TypeSourceCapture::live(reader(move |_, max| {
        observed.lock().unwrap().push(max);
        Ok("x".repeat(max_package_bytes()))
    }));
    for index in 0..7 {
        capture.read(&index.to_string(), token()).await.unwrap();
    }
    assert_eq!(
        capture.read("7", token()).await.unwrap_err(),
        TypeSourceError::Capacity
    );
    assert_eq!(
        limits.lock().unwrap().last(),
        Some(&(max_package_bytes() - 8))
    );
    capture.read("0", token()).await.unwrap();
    assert_eq!(limits.lock().unwrap().len(), 8);
}

#[test]
fn replay_rejects_oversized_or_invalid_snapshots_before_exposing_any_entries() {
    for (sources, expected) in [
        (
            IndexMap::from([("a".into(), "x".repeat(max_package_bytes() + 1))]),
            TypeSourceError::TooLarge,
        ),
        (
            (0..8)
                .map(|i| (i.to_string(), "x".repeat(max_package_bytes())))
                .collect(),
            TypeSourceError::Capacity,
        ),
        (
            (0..257).map(|i| (i.to_string(), String::new())).collect(),
            TypeSourceError::Capacity,
        ),
        (
            IndexMap::from([("bad\0path".into(), String::new())]),
            TypeSourceError::InvalidPath,
        ),
    ] {
        assert!(matches!(TypeSourceCapture::replay(sources), Err(error) if error == expected));
    }
}

#[tokio::test]
async fn cancellation_before_entry_or_a_cache_hit_returns_no_package() {
    let calls = Arc::new(AtomicUsize::new(0));
    let reads = calls.clone();
    let mut capture = TypeSourceCapture::live(reader(move |_, _| {
        reads.fetch_add(1, Ordering::SeqCst);
        Ok("content".into())
    }));
    let cancelled = token();
    cancelled.cancel();
    assert_eq!(
        capture.read("a", cancelled.clone()).await.unwrap_err(),
        TypeSourceError::Cancelled
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    capture.read("a", token()).await.unwrap();
    assert_eq!(
        capture.read("a", cancelled).await.unwrap_err(),
        TypeSourceError::Cancelled
    );
    assert!(capture.finish().is_empty());
}

#[tokio::test]
async fn cancellation_joins_physical_reader_exit_and_does_not_cache_its_content() {
    let (entered, entered_rx) = oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let mut capture = TypeSourceCapture::live(reader(move |_, _| {
        entered.lock().unwrap().take().unwrap().send(()).unwrap();
        released
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        Ok("content".into())
    }));
    let cancellation = token();
    let signal = cancellation.clone();
    let mut worker = tokio::spawn(async move {
        let result = capture.read("types.yaml", cancellation).await;
        (result, capture.finish())
    });
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    signal.cancel();
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut worker)
            .await
            .is_err()
    );
    release.send(()).unwrap();
    let (result, sources) = worker.await.unwrap();
    assert_eq!(result.unwrap_err(), TypeSourceError::Cancelled);
    assert!(sources.is_empty());
}

#[tokio::test]
async fn a_panicking_reader_never_exposes_its_payload_as_a_package_error() {
    let mut capture = TypeSourceCapture::live(reader(|_, _| panic!("private reader payload")));
    let error = capture.read("private-path", token()).await.unwrap_err();
    assert_eq!(error, TypeSourceError::Worker);
    assert!(!format!("{error:?}: {error}").contains("private"));
    assert!(capture.finish().is_empty());
}
