use super::*;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

// One wall-clock deadline covers parsing and pipe operations. Downloads
// have their own shorter network budget; cancellation is independently selectable.
fn extraction_budget() -> Duration {
    Duration::from_millis(wes_budgets::get("api.extract.ms"))
}

struct ExtractionFailure(String, &'static str, Option<ExtractionReport>);
impl std::fmt::Debug for ExtractionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtractionFailure")
            .field("code", &self.1)
            .field("message", &self.0)
            .finish_non_exhaustive()
    }
}

/// Untrusted, private document diagnostics. Never include this in Display/public errors.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionReport {
    pub version: u8,
    pub message: String,
    pub issues: Vec<ExtractionIssue>,
    pub omitted: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionIssue {
    pub kind: String,
    pub operation: String,
    pub message: String,
    pub lines: Vec<ExtractionLines>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractionLines {
    pub start: usize,
    pub end: usize,
}
impl ExtractionReport {
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        crate::codec::decode_json_preserving(
            bytes,
            crate::codec::Limits {
                bytes: 256 * 1024,
                nodes: 10_000,
            },
        )
        .ok()?;
        let report: Self = serde_json::from_slice(bytes).ok()?;
        let text = |s: &str| {
            s.len() <= 2048
                && !s
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
        };
        (report.version == 1
            && text(&report.message)
            && report.issues.len() <= 32
            && report.omitted <= 10_000
            && report.issues.iter().all(|issue| {
                matches!(
                    issue.kind.as_str(),
                    "missing" | "conflict" | "unsupported" | "advisory" | "blocked" | "validation"
                ) && text(&issue.operation)
                    && text(&issue.message)
                    && issue.lines.len() <= 16
                    && issue.lines.iter().all(|l| {
                        l.start > 0 && l.end >= l.start && l.end <= 50_000 && l.end - l.start < 64
                    })
            }))
        .then_some(report)
    }
}
pub fn extraction_report(error: &io::Error) -> Option<&ExtractionReport> {
    error
        .get_ref()?
        .downcast_ref::<ExtractionFailure>()?
        .2
        .as_ref()
}
impl std::fmt::Display for ExtractionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ExtractionFailure {}
/// Return only known diagnostics, never subprocess stderr or arbitrary error text.
pub fn extraction_message(error: &io::Error) -> &str {
    error.get_ref().and_then(|e| e.downcast_ref::<ExtractionFailure>()).map_or(
        "wes-describe failed or produced an invalid definition; inspect its standalone diagnostics", |e| e.0.as_str())
}
pub fn extraction_code(error: &io::Error) -> &'static str {
    error
        .get_ref()
        .and_then(|e| e.downcast_ref::<ExtractionFailure>())
        .map_or("DSC005", |e| e.1)
}
fn exit_code(code: Option<i32>) -> &'static str {
    match code {
        Some(21) => "DSC002",
        Some(22) => "DSC005",
        _ => "DSC004",
    }
}
fn exit_message(code: Option<i32>) -> &'static str {
    match code {
        Some(21) => {
            "API source contains unsupported or invalid declarations; inspect import details"
        }
        Some(22) => "API conversion failed native contract validation; no revision was saved",
        _ => "API conversion failed; inspect import details",
    }
}

fn timeout_failure(budget: Duration) -> ExtractionFailure {
    ExtractionFailure(
        format!(
            "wes-describe exceeded its {}-second time budget; no revision was saved",
            budget.as_secs()
        ),
        "DSC007",
        None,
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Extraction {
    pub location: String,
    #[serde(default)]
    pub allow_partial: bool,
}
async fn read_pipe(mut pipe: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    (&mut pipe)
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > limit {
        return Err(error("extractor output exceeds its byte budget"));
    }
    Ok(bytes)
}
pub async fn extract(
    executable: &Path,
    key: &PackageKey,
    source: &[u8],
    options: &Extraction,
) -> Result<Vec<u8>> {
    extract_output(
        executable,
        key,
        source,
        options,
        wes_engine::driver::CancellationToken::new(),
        false,
        extraction_budget(),
    )
    .await
}
pub async fn extract_draft_controlled(
    executable: &Path,
    key: &PackageKey,
    source: &[u8],
    options: &Extraction,
    cancellation: wes_engine::driver::CancellationToken,
) -> Result<Vec<u8>> {
    extract_output(
        executable,
        key,
        source,
        options,
        cancellation,
        true,
        extraction_budget(),
    )
    .await
}
async fn extract_output(
    executable: &Path,
    key: &PackageKey,
    source: &[u8],
    options: &Extraction,
    cancellation: wes_engine::driver::CancellationToken,
    editable: bool,
    budget: Duration,
) -> Result<Vec<u8>> {
    if !executable.is_absolute() || !executable.is_file() {
        return Err(error(
            "configure an absolute wes-extract executable path before ingestion",
        ));
    }
    let mut provider = key.service.replace(['.', '-'], "_");
    if provider.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        provider = format!("api_{provider}");
    }
    let mut command = Command::new(executable);
    command
        .env_clear()
        .args(["-provider", &provider, "-from", "-"]);
    if editable {
        command.arg("-draft");
    }
    if options.allow_partial {
        command.arg("-allow-partial");
    }
    command
        .arg("-diagnostics-json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if cancellation.is_cancelled() {
        return Err(error("describe cancelled"));
    }
    #[cfg(unix)]
    command.process_group(0);
    let deadline = tokio::time::Instant::now() + budget;
    let starting = crate::process::serialized_spawn_async(|| {
        if cancellation.is_cancelled() {
            return Err(error("describe cancelled"));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(io::Error::other(timeout_failure(budget)));
        }
        command
            .spawn()
            .map_err(|_| error("extractor could not be launched"))
    });
    let mut child = tokio::select! {
        biased;
        _ = cancellation.cancelled() => return Err(error("describe cancelled")),
        result = tokio::time::timeout_at(deadline, starting) => {
            result.map_err(|_| io::Error::other(timeout_failure(budget)))??
        }
    };
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let process_id = child.id();
    let output_stop = wes_engine::driver::CancellationToken::new();
    let stop_reading = output_stop.clone();
    // Keep the bounded reader owned and join it on cancellation.
    let mut output_task = tokio::spawn(async move {
        tokio::select! {
            _ = stop_reading.cancelled() => Err(error("extractor output reading stopped")),
            result = read_pipe(stdout, max_descriptor()) => result,
        }
    });
    let result = tokio::select! {
      _ = cancellation.cancelled() => None,
      result = tokio::time::timeout_at(deadline, async {
        tokio::try_join!(
            child.wait(),
            async { (&mut output_task).await.map_err(|_| error("extractor output worker failed"))? },
            read_pipe(stderr, 128 * 1024),
            async {
                // An early rejection may close stdin before the document fits the pipe.
                // Preserve its exit category instead of replacing it with BrokenPipe.
                match stdin.write_all(source).await {
                    Ok(()) => stdin.shutdown().await?,
                    Err(e) if e.kind() == io::ErrorKind::BrokenPipe => (),
                    Err(e) => return Err(e),
                }
                drop(stdin);
                Ok::<(), io::Error>(())
            }
        )
    })
    => Some(result),
    };
    match result {
        Some(Ok(Ok((status, bytes, _, ())))) if status.success() => {
            let validation = if editable {
                super::draft::extraction(&bytes).map(|_| ())
            } else {
                validate_descriptor(&bytes).map(|_| ())
            };
            validation.map_err(|error| {
                let mut message = error.to_string();
                if message.len() > 2048 {
                    let mut end = 2045;
                    while !message.is_char_boundary(end) {
                        end -= 1;
                    }
                    message.truncate(end);
                    message.push('…');
                }
                io::Error::other(ExtractionFailure(
                    exit_message(Some(22)).into(),
                    "DSC005",
                    Some(ExtractionReport {
                        version: 1,
                        message,
                        issues: vec![],
                        omitted: 0,
                    }),
                ))
            })?;
            Ok(bytes)
        }
        failure => {
            let report = match &failure {
                Some(Ok(Ok((status, bytes, _, ())))) if matches!(status.code(), Some(21 | 22)) => {
                    ExtractionReport::decode(bytes)
                }
                _ => None,
            };
            let code = match &failure {
                Some(Err(_)) => "DSC007",
                Some(Ok(Ok((status, _, _, ())))) => exit_code(status.code()),
                _ => "DSC004",
            };
            let retention = "no revision was saved";
            let message: String = match failure {
                None => format!("describe cancelled; {retention}"),
                Some(Err(_)) => timeout_failure(budget).0,
                Some(Ok(Ok((status, _, _, ())))) => exit_message(status.code()).into(),
                _ => {
                    format!(
                        "wes-describe failed while reading or writing bounded process data; {retention}"
                    )
                }
            };
            // Stop pending reads and join the owned process group.
            output_stop.cancel();
            #[cfg(unix)]
            if let Some(pid) = process_id.and_then(|id| rustix::process::Pid::from_raw(id as i32)) {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
            }
            let _ = child.kill().await;
            let _ = child.wait().await;
            if !output_task.is_finished() {
                let _ = output_task.await;
            }
            Err(io::Error::other(ExtractionFailure(message, code, report)))
        }
    }
}

#[cfg(test)]
mod diagnostics_tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn queued_extraction_honors_cancel_and_budget_without_starting_a_child() {
        use std::{os::unix::fs::PermissionsExt, sync::mpsc};
        use wes_engine::driver::CancellationToken;
        for cancel in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let executable = dir.path().join("extractor");
            let marker = dir.path().join("entered");
            std::fs::write(
                &executable,
                format!(
                    "#!/bin/sh\necho entered > '{}'\nexit 21\n",
                    marker.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let (ready_tx, ready_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let owner = std::thread::spawn(move || {
                crate::process::serialized_spawn(|| {
                    ready_tx.send(()).unwrap();
                    let _ = release_rx.recv_timeout(Duration::from_secs(2));
                })
            });
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let key = PackageKey {
                service: "fixture".into(),
                api_version: "v1".into(),
                scope: "test".into(),
            };
            let options = Extraction {
                location: "synthetic".into(),
                allow_partial: false,
            };
            let token = CancellationToken::new();
            let work = extract_output(
                &executable,
                &key,
                b"synthetic",
                &options,
                token.clone(),
                true,
                if cancel {
                    Duration::from_secs(5)
                } else {
                    Duration::from_millis(50)
                },
            );
            let result = tokio::time::timeout(Duration::from_secs(1), async {
                if cancel {
                    let (result, ()) = tokio::join!(work, async {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        token.cancel();
                    });
                    result
                } else {
                    work.await
                }
            })
            .await;
            let _ = release_tx.send(());
            owner.join().unwrap();
            let error = result
                .expect("queued extraction ignored cancellation or budget")
                .unwrap_err();
            if cancel {
                assert!(error.to_string().contains("cancelled"));
            } else {
                assert_eq!(extraction_code(&error), "DSC007");
            }
            assert!(
                !marker.exists(),
                "a cancelled or expired queue entry launched a child"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn deadline_and_cancellation_join_the_process_group() {
        use std::os::unix::fs::PermissionsExt;
        use wes_engine::driver::CancellationToken;

        for cancel in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let executable = dir.path().join("extractor");
            let marker = dir.path().join("pids");
            std::fs::write(
                &executable,
                format!(
                    "#!/bin/sh\n/bin/cat >/dev/null\n/bin/sleep 60 &\necho $$ $! > '{}'\nwait\n",
                    marker.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let token = CancellationToken::new();
            let key = PackageKey {
                service: "fixture".into(),
                api_version: "v1".into(),
                scope: "test".into(),
            };
            let options = Extraction {
                location: "synthetic".into(),
                allow_partial: false,
            };
            let budget = if cancel {
                extraction_budget()
            } else {
                // A newly created executable can take several seconds to launch on macOS.
                // Keep the real deadline assertion, with room for that startup.
                Duration::from_secs(5)
            };
            let work = extract_output(
                &executable,
                &key,
                b"synthetic",
                &options,
                token.clone(),
                true,
                budget,
            );
            let observe = async {
                let pids = tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if let Ok(text) = std::fs::read_to_string(&marker) {
                            let pids: Vec<i32> = text
                                .split_whitespace()
                                .filter_map(|s| s.parse().ok())
                                .collect();
                            if pids.len() == 2 {
                                break pids;
                            }
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
                if cancel {
                    token.cancel();
                }
                pids
            };
            let (result, pids) = tokio::time::timeout(Duration::from_secs(10), async {
                tokio::join!(work, observe)
            })
            .await
            .unwrap();
            let error = result.unwrap_err();
            if cancel {
                assert!(extraction_message(&error).contains("cancelled"));
                assert_ne!(extraction_code(&error), "DSC007");
            } else {
                assert_eq!(extraction_code(&error), "DSC007");
                assert!(extraction_message(&error).contains("5-second"));
            }
            // Direct child is joined; allow the OS to reap the terminated descendant.
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if pids.iter().all(|pid| {
                        rustix::process::test_kill_process(
                            rustix::process::Pid::from_raw(*pid).unwrap(),
                        )
                        .is_err()
                    }) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
    }

    #[test]
    fn structured_reports_are_private_bounded_and_strict() {
        let valid = serde_json::json!({"version":1,"message":"private fact","issues":[{"kind":"missing","operation":"GET /items","message":"schema missing","lines":[{"start":2,"end":3}]}],"omitted":0});
        let decode =
            |v: &serde_json::Value| ExtractionReport::decode(&serde_json::to_vec(v).unwrap());
        let report = decode(&valid).unwrap();
        let e = io::Error::other(ExtractionFailure(
            exit_message(Some(21)).into(),
            "DSC002",
            Some(report),
        ));
        assert!(!e.to_string().contains("private fact"));
        assert_eq!(extraction_report(&e).unwrap().message, "private fact");
        for invalid in [
            serde_json::json!({"version":1,"version":2}),
            {
                let mut v = valid.clone();
                v["stderr"] = serde_json::json!("secret");
                v
            },
            {
                let mut v = valid.clone();
                v["message"] = serde_json::json!("x".repeat(2049));
                v
            },
            {
                let mut v = valid.clone();
                v["issues"][0]["lines"][0]["end"] = serde_json::json!(50001);
                v
            },
            {
                let mut v = valid.clone();
                v["issues"] = serde_json::json!(vec![valid["issues"][0].clone(); 33]);
                v
            },
        ] {
            assert!(decode(&invalid).is_none());
        }
        assert!(
            ExtractionReport::decode(
                br#"{"version":1,"version":1,"message":"x","issues":[],"omitted":0}"#
            )
            .is_none()
        );
        assert!(ExtractionReport::decode(b"raw secret stderr").is_none());
    }
    #[test]
    fn only_fixed_failure_categories_are_exposed() {
        for code in [1, 21, 22] {
            let message = exit_message(Some(code));
            let error = io::Error::other(ExtractionFailure(
                message.into(),
                exit_code(Some(code)),
                None,
            ));
            assert_eq!(extraction_message(&error), message);
        }
        assert!(
            !extraction_message(&io::Error::other("untrusted secret stderr")).contains("secret")
        );
        assert_ne!(exit_message(Some(20)), exit_message(Some(21)));
        assert_ne!(exit_message(Some(21)), exit_message(Some(22)));
    }
}
