//! Lazy local socket selection. Once selected, a binding never switches daemons.
use super::*;
use std::path::PathBuf;
use tokio::sync::OnceCell;
use wes_engine::{
    environments::Authority,
    streams::{StreamFuture, StreamSink, StreamingInvoker},
};

pub fn local(alias: &str) -> Result<ImportProduct, wes_engine::imports::ImportError> {
    with_candidates(alias, host_candidates())
}
pub fn with_candidates(
    alias: &str,
    candidates: Vec<PathBuf>,
) -> Result<ImportProduct, wes_engine::imports::ImportError> {
    struct Factory {
        alias: String,
        candidates: Vec<PathBuf>,
    }
    impl wes_engine::imports::EnvironmentFactory for Factory {
        fn bind(
            &self,
            binding: &Binding,
            authority: Authority,
        ) -> Result<ImportProduct, wes_engine::imports::ImportError> {
            build(&self.alias, binding, authority, &self.candidates)
                .map_err(wes_engine::imports::ImportError::Input)
        }
    }
    super::unconnected(alias).map(|product| {
        product.with_environment_factory(Arc::new(Factory {
            alias: alias.into(),
            candidates,
        }))
    })
}

/// Fixed local candidates; no CLI invocation, Docker context or remote host inheritance.
pub(crate) fn host_candidates() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("/var/run/docker.sock")];
    if let Some(home) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        #[cfg(target_os = "macos")]
        paths.push(home.join(".docker/run/docker.sock"));
        #[cfg(target_os = "linux")]
        paths.push(home.join(".docker/desktop/docker.sock"));
    }
    #[cfg(target_os = "linux")]
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        paths.push(runtime.join("docker.sock"));
    }
    paths
}

#[derive(Clone)]
struct Automatic {
    alias: String,
    candidates: Arc<[PathBuf]>,
    binding: Binding,
    authority: Authority,
    selected: Arc<OnceCell<ImportProduct>>,
}

pub(crate) fn build(
    alias: &str,
    binding: &Binding,
    authority: Authority,
    candidates: &[PathBuf],
) -> Result<ImportProduct, &'static str> {
    if candidates.is_empty()
        || candidates.len() > 4
        || candidates.iter().any(|p| {
            !p.is_absolute()
                || p.to_str()
                    .is_none_or(|s| s.len() > 4096 || s.chars().any(char::is_control))
        })
    {
        return Err("Docker automatic discovery requires 1–4 absolute local socket candidates");
    }
    let invoker = Arc::new(Automatic {
        alias: alias.into(),
        candidates: candidates.into(),
        binding: binding.clone(),
        authority,
        selected: Arc::new(OnceCell::new()),
    });
    ImportProduct::new(observation::description(alias)?, invoker.clone(), vec![])
        .map(|product| product.with_streams(invoker))
        .map_err(|_| "Docker automatic discovery metadata exceeds budget")
}

impl Automatic {
    async fn select(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<&ImportProduct, InvocationError> {
        if cancellation.is_cancelled() {
            return Err(InvocationError::Cancelled);
        }
        self.selected
            .get_or_try_init(|| async {
                let candidates = self.candidates.clone();
                let socket = tokio::task::spawn_blocking(move || select_socket(&candidates))
                    .await
                    .map_err(|_| {
                        observation::failure(
                            "DOCKER_CONNECTION",
                            "Could not inspect local Docker socket locations",
                        )
                    })??;
                if cancellation.is_cancelled() {
                    return Err(InvocationError::Cancelled);
                }
                observation::build_at(&self.alias, &socket, &self.binding, self.authority.clone())
                    .map_err(|message| observation::failure("DOCKER_CONNECTION", message))
            })
            .await
    }
}
impl Invoker for Automatic {
    fn invoke(&self, call: Call, cancellation: CancellationToken) -> InvocationFuture {
        let this = self.clone();
        Box::pin(async move {
            let selected = this.select(&cancellation).await?;
            selected.invoker().invoke(call, cancellation).await
        })
    }
}
impl StreamingInvoker for Automatic {
    fn subscribe(
        &self,
        call: Call,
        sink: StreamSink,
        cancellation: CancellationToken,
    ) -> StreamFuture {
        let this = self.clone();
        Box::pin(async move {
            let selected = this.select(&cancellation).await?;
            selected
                .streams()
                .expect("Docker streaming port")
                .subscribe(call, sink, cancellation)
                .await
        })
    }
}

fn select_socket(candidates: &[PathBuf]) -> Result<String, InvocationError> {
    let mut sockets = std::collections::BTreeSet::new();
    let mut checked = Vec::new();
    for candidate in candidates {
        let state = match std::fs::metadata(candidate) {
            Ok(metadata) if is_socket(&metadata) => {
                let canonical = std::fs::canonicalize(candidate).map_err(|_| observation::failure(
                    "DOCKER_CONNECTION", format!("Cannot resolve Docker socket {}. Check its permissions or set an explicit endpoint in /edit env.", candidate.display())))?;
                sockets.insert(canonical);
                "socket"
            }
            Ok(_) => "not a Unix socket",
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "not found",
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                "permission denied"
            }
            Err(_) => "unavailable",
        };
        checked.push(format!("{} ({state})", candidate.display()));
    }
    match sockets.len() {
        1 => sockets
            .into_iter()
            .next()
            .and_then(|p| p.into_os_string().into_string().ok())
            .ok_or_else(|| {
                observation::failure(
                    "DOCKER_CONNECTION",
                    "Docker socket path is not valid UTF-8; set an explicit endpoint",
                )
            }),
        0 => Err(observation::failure(
            "DOCKER_CONNECTION",
            format!(
                "No local Docker socket found. Checked: {}. Start Docker, or use /edit env to set bind: {{target: local, endpoint: unix:///absolute/path/to/docker.sock}}. No Docker operation was sent.",
                checked.join(", ")
            ),
        )),
        _ => Err(observation::failure(
            "DOCKER_CONNECTION",
            format!(
                "Multiple local Docker sockets found: {}. Choose one with an explicit endpoint in /edit env. No Docker operation was sent.",
                sockets
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )),
    }
}
#[cfg(unix)]
fn is_socket(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    metadata.file_type().is_socket()
}
#[cfg(not(unix))]
fn is_socket(_: &std::fs::Metadata) -> bool {
    false
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::{fs::symlink, net::UnixListener};
    #[test]
    fn selection_is_local_bounded_and_deduplicates_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let missing = root.path().join("missing.sock");
        let error = select_socket(&[missing.clone()]).unwrap_err().to_string();
        assert!(error.contains("No local Docker socket found"));
        assert!(error.contains("/edit env"));
        let socket = root.path().join("docker.sock");
        let _first = UnixListener::bind(&socket).unwrap();
        let link = root.path().join("default.sock");
        symlink(&socket, &link).unwrap();
        assert_eq!(
            select_socket(&[missing, socket.clone(), link]).unwrap(),
            socket.canonicalize().unwrap().to_str().unwrap()
        );
        let other = root.path().join("other.sock");
        let _second = UnixListener::bind(&other).unwrap();
        assert!(
            select_socket(&[socket, other])
                .unwrap_err()
                .to_string()
                .contains("Multiple local Docker sockets")
        );
    }
}
