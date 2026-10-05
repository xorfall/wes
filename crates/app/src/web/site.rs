//! Startup-only bounded site image. URL lookup never performs filesystem I/O.
use axum::{
    body::Bytes,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use std::{
    collections::BTreeMap,
    io::{self, Read},
    path::PathBuf,
};
#[derive(Default)]
pub(super) struct Site {
    files: BTreeMap<String, (Bytes, &'static str)>,
    bytes: usize,
    entries: usize,
}
impl Site {
    pub fn load(path: Option<PathBuf>) -> io::Result<Self> {
        let mut site = Self::default();
        if let Some(path) = path {
            let dir = Dir::open_ambient_dir(path, ambient_authority())?;
            site.read(&dir, "", 0)?;
            if !site.files.contains_key("/index.html") {
                return Err(io::Error::other("site has no index.html"));
            }
        }
        Ok(site)
    }
    fn read(&mut self, directory: &Dir, prefix: &str, depth: usize) -> io::Result<()> {
        if depth > 16 {
            return Err(io::Error::other("site directory nesting limit"));
        }
        for entry in directory.entries()? {
            self.entries += 1;
            if self.entries > 1024 {
                return Err(io::Error::other("site entry limit"));
            }
            let entry = entry?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| io::Error::other("site filename is not UTF-8"))?;
            // Exact URL map: refuse names that require decoding or ambiguously normalize.
            if name.contains(['%', '\\', '?', '#']) {
                return Err(io::Error::other("unsupported site filename"));
            }
            let kind = entry.file_type()?;
            let path = format!("{prefix}/{name}");
            if kind.is_dir() {
                let dir = directory.open_dir_nofollow(&name)?;
                self.read(&dir, &path, depth + 1)?;
            } else if kind.is_file() {
                let mut options = OpenOptions::new();
                options.read(true).follow(FollowSymlinks::No).nonblock(true);
                let file = directory.open_with(&name, &options)?;
                if !file.metadata()?.is_file() {
                    return Err(io::Error::other("site entry is not a regular file"));
                }
                let remaining = (32 * 1024 * 1024usize).saturating_sub(self.bytes);
                let mut bytes = Vec::new();
                file.take(remaining as u64 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > remaining {
                    return Err(io::Error::other("site byte limit"));
                }
                self.bytes += bytes.len();
                let content = if name.ends_with(".html") {
                    "text/html; charset=utf-8"
                } else if name.ends_with(".js") {
                    "text/javascript; charset=utf-8"
                } else if name.ends_with(".css") {
                    "text/css; charset=utf-8"
                } else if name.ends_with(".svg") {
                    "image/svg+xml"
                } else if name.ends_with(".png") {
                    "image/png"
                } else if name.ends_with(".woff2") {
                    "font/woff2"
                } else {
                    "application/octet-stream"
                };
                self.files.insert(path, (bytes.into(), content));
            } else {
                return Err(io::Error::other(
                    "site entries must be regular files or directories",
                ));
            }
        }
        Ok(())
    }
    pub fn response(&self, path: &str) -> Response {
        let path = if path == "/" { "/index.html" } else { path };
        match self.files.get(path) {
            Some((bytes, kind)) => ([(header::CONTENT_TYPE, *kind)], bytes.clone()).into_response(),
            None if self.files.is_empty() && path == "/index.html" => (
                StatusCode::NOT_FOUND,
                "Wes API server is running without the browser interface.\nBuild the client with: cd gui && npm ci && npm run build\nThen start the server from the checkout root with --site gui/dist.\n",
            ).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn absent_site_explains_how_to_start_the_interface() {
        let site = Site::load(None).unwrap();
        let response = site.response("/");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("--site gui/dist"));
        assert!(text.contains("npm run build"));
        let other = site.response("/absent.js");
        assert_eq!(
            axum::body::to_bytes(other.into_body(), 4096)
                .await
                .unwrap()
                .len(),
            0
        );
    }
}
