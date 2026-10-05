use super::*;
use cap_std::{ambient_authority, fs::Dir};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Repository {
    Local {
        directory: PathBuf,
    },
    Github {
        owner: String,
        repository: String,
        commit: String,
        #[serde(default)]
        prefix: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryPackage {
    pub key: PackageKey,
    pub revision: String,
    pub manifest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryCatalog {
    pub version: u32,
    pub packages: Vec<RepositoryPackage>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    key: PackageKey,
    revision: String,
    descriptor: String,
    reviewed: bool,
    source: Option<SourceFile>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceFile {
    path: String,
    sha256: String,
}
pub struct RepositoryArtifact {
    pub bytes: Vec<u8>,
    pub source: Option<Vec<u8>>,
    pub reviewed: bool,
}
fn relative(path: &str) -> Result<()> {
    if path.len() > 1024 || path.is_empty() || !path.split('/').all(identifier) {
        return Err(error(
            "repository paths require bounded relative ASCII components",
        ));
    }
    Ok(())
}
impl Repository {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Local { directory } => {
                validate_directory(directory)?;
                if !directory.is_dir() {
                    return Err(error("repository directory unavailable"));
                }
            }
            Self::Github {
                owner,
                repository,
                commit,
                prefix,
            } => {
                if !identifier(owner)
                    || !identifier(repository)
                    || commit.len() != 40
                    || !commit
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(error(
                        "GitHub requires owner/repository and a full 40-character lowercase commit SHA",
                    ));
                }
                if !prefix.is_empty() {
                    relative(prefix)?;
                }
            }
        }
        Ok(())
    }
    pub fn origin(&self) -> String {
        match self {
            Self::Local { directory } => format!("local-repository:{}", directory.display()),
            Self::Github {
                owner,
                repository,
                commit,
                prefix,
            } => format!("github:{owner}/{repository}@{commit}/{prefix}"),
        }
    }
    async fn read(&self, path: &str, limit: usize) -> Result<Vec<u8>> {
        self.validate()?;
        relative(path)?;
        match self {
            Self::Local { directory } => {
                let dir = Dir::open_ambient_dir(directory, ambient_authority())?;
                files::read_confined(&dir, path, limit)
            }
            Self::Github {
                owner,
                repository,
                commit,
                prefix,
            } => {
                let full = if prefix.is_empty() {
                    path.into()
                } else {
                    format!("{prefix}/{path}")
                };
                let mut url = reqwest::Url::parse(&format!(
                    "https://api.github.com/repos/{owner}/{repository}/contents/{full}"
                ))
                .map_err(|_| error("invalid GitHub repository path"))?;
                url.query_pairs_mut().append_pair("ref", commit);
                fetch(url, limit, true).await
            }
        }
    }
    pub async fn catalog(&self) -> Result<RepositoryCatalog> {
        let catalog: RepositoryCatalog =
            files::decode_metadata(&self.read("catalog.json", max_source()).await?)?;
        if catalog.version != 1 || catalog.packages.len() > 4000 {
            return Err(error("unsupported or oversized repository catalog"));
        }
        let mut seen = std::collections::HashSet::new();
        for entry in &catalog.packages {
            entry.key.validate()?;
            relative(&entry.manifest)?;
            if !valid_hash(&entry.revision)
                || !seen.insert((
                    entry.key.service.clone(),
                    entry.key.api_version.clone(),
                    entry.key.scope.clone(),
                    entry.revision.clone(),
                ))
            {
                return Err(error("invalid or duplicate repository package"));
            }
        }
        Ok(catalog)
    }
    pub async fn download(&self, entry: &RepositoryPackage) -> Result<RepositoryArtifact> {
        let manifest: Manifest =
            files::decode_metadata(&self.read(&entry.manifest, 64 * 1024).await?)?;
        if manifest.version != 1 || manifest.key != entry.key || manifest.revision != entry.revision
        {
            return Err(error("repository manifest identity does not match catalog"));
        }
        let bytes = self.read(&manifest.descriptor, max_descriptor()).await?;
        if digest(&bytes) != manifest.revision {
            return Err(error("repository descriptor SHA-256 mismatch"));
        }
        validate_descriptor(&bytes)?;
        let source = match manifest.source {
            None => None,
            Some(source) => {
                let bytes = self.read(&source.path, max_source()).await?;
                if !valid_hash(&source.sha256)
                    || digest(&bytes) != source.sha256
                    || std::str::from_utf8(&bytes).is_err()
                {
                    return Err(error("repository source integrity/UTF-8 check failed"));
                }
                Some(bytes)
            }
        };
        Ok(RepositoryArtifact {
            bytes,
            source,
            reviewed: manifest.reviewed,
        })
    }
}
async fn fetch(url: reqwest::Url, limit: usize, github: bool) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|_| error("HTTP reader unavailable"))?;
    let mut request = client.get(url).header("User-Agent", "wes-api-library");
    if github {
        request = request
            .header("Accept", "application/vnd.github.raw+json")
            .header("X-GitHub-Api-Version", "2026-03-10");
    } else {
        request = request.header("Accept", super::DOCUMENT_ACCEPT);
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| error("repository/document download failed; no extraction fallback"))?;
    if !response.status().is_success() {
        return Err(error(&format!(
            "repository/document returned HTTP {}; no extraction fallback",
            response.status().as_u16()
        )));
    }
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(error("download exceeds its byte budget"));
    }
    let mut bytes = vec![];
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| error("download interrupted"))?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(error("download exceeds its byte budget"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
pub async fn read_document(location: &str) -> Result<Vec<u8>> {
    let bytes = if location.starts_with("http://") || location.starts_with("https://") {
        let url = reqwest::Url::parse(location).map_err(|_| error("invalid documentation URL"))?;
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(error(
                "documentation URL must not contain credentials, query or fragment",
            ));
        }
        fetch(url, max_source(), false).await?
    } else {
        let path = Path::new(location);
        if !path.is_absolute() {
            return Err(error("documentation path must be absolute"));
        }
        let parent = path.parent().ok_or_else(|| error("invalid source path"))?;
        let dir = Dir::open_ambient_dir(parent, ambient_authority())?;
        files::read_confined(
            &dir,
            path.file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| error("invalid source filename"))?,
            max_source(),
        )?
    };
    if bytes.is_empty() || std::str::from_utf8(&bytes).is_err() {
        return Err(error("documentation must be nonempty UTF-8"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    #[tokio::test]
    async fn documentation_negotiates_markdown_and_preserves_full_publisher_bytes() {
        for body in [
            "# Complete synthetic API\nPOST /items\n",
            "{\"openapi\":\"3.0.3\",\"paths\":{}}",
            "<html>HTML fallback remains supported</html>",
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 8192];
                let n = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..n]).to_lowercase();
                assert!(request.contains(&format!("accept: {}", super::super::DOCUMENT_ACCEPT)));
                assert!(!request.contains("authorization:"));
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            });
            assert_eq!(
                read_document(&format!("http://{address}/docs"))
                    .await
                    .unwrap(),
                body.as_bytes()
            );
            server.await.unwrap();
        }
    }
    #[tokio::test]
    async fn github_transport_uses_only_get_and_refuses_failures_redirects_and_excess_bytes() {
        for (status, body, limit, valid) in [
            ("200 OK", "{}", 10, true),
            ("404 Not Found", "missing", 100, false),
            ("302 Found", "redirect", 100, false),
            ("200 OK", "too large", 3, false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = vec![0; 8192];
                let n = socket.read(&mut bytes).await.unwrap();
                let request = String::from_utf8_lossy(&bytes[..n]).to_lowercase();
                assert!(
                    request.starts_with("get /repos/example/catalog/contents/catalog.json?ref=")
                );
                assert!(request.contains("accept: application/vnd.github.raw+json"));
                assert!(request.contains("x-github-api-version: 2026-03-10"));
                assert!(!request.contains("authorization:"));
                socket.write_all(reply.as_bytes()).await.unwrap();
            });
            let url = reqwest::Url::parse(&format!(
                "http://{address}/repos/example/catalog/contents/catalog.json?ref={}",
                "a".repeat(40)
            ))
            .unwrap();
            assert_eq!(fetch(url, limit, true).await.is_ok(), valid);
            server.await.unwrap();
        }
    }
}
