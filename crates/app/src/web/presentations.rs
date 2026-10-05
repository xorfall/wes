//! `GET /presentations`: the data home's `presentations/` directory, served read-only.
//!
//! Transport only. The engine does not parse, validate or interpret an entry: it reads the YAML
//! files as text, within fixed bounds, through a confined directory handle, and hands them to the
//! client, which validates them (type name, declared fields, formatters) and keeps the last valid
//! entry of a file that stops validating. Nothing here changes a node's state, error, privacy or
//! authority.
//!
//! Change notices are long polls: `?after=<revision>` waits until the directory's content revision
//! differs from the one the client holds (or a bounded wait passes) and then answers with the whole
//! listing, so a client never acts on half a directory.
use super::*;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};

/// Files served at most; the rest are named as problems.
fn max_files() -> usize {
    wes_budgets::get("ui.presentations.files") as usize
}
/// Bytes one entry may take; entries are a few lines.
fn max_file_bytes() -> u64 {
    wes_budgets::get("ui.presentation.bytes") as u64
}
/// How long a long poll waits for a change before answering with the unchanged listing.
const MAX_WAIT: Duration = Duration::from_secs(25);
/// How often a long poll looks at the directory again.
const POLL: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct File {
    pub name: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct Problem {
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct Listing {
    /// Content identity of the directory as served: changes when any served byte or name changes.
    pub revision: String,
    pub files: Vec<File>,
    pub problems: Vec<Problem>,
}

fn is_entry_name(name: &str) -> bool {
    (name.ends_with(".yaml") || name.ends_with(".yml")) && !name.starts_with('.')
}

/// Reads the directory now. A missing directory is an empty listing, not an error.
pub(super) fn read_listing(path: &Path) -> io::Result<Listing> {
    let mut files = Vec::new();
    let mut problems = Vec::new();
    let directory = match Dir::open_ambient_dir(path, ambient_authority()) {
        Ok(directory) => Some(directory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(directory) = directory {
        let mut names = Vec::new();
        for entry in directory.entries()? {
            let entry = entry?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if is_entry_name(&name) {
                names.push((name, entry.file_type()?));
            }
        }
        names.sort_by(|a, b| a.0.cmp(&b.0));
        for (at, (name, kind)) in names.into_iter().enumerate() {
            if at >= max_files() {
                problems.push(Problem {
                    name,
                    message: format!("more than {} presentation files", max_files()),
                });
                continue;
            }
            if !kind.is_file() {
                problems.push(Problem {
                    name,
                    message: "not a regular file".into(),
                });
                continue;
            }
            match read_entry(&directory, &name) {
                Ok(text) => files.push(File { name, text }),
                Err(message) => problems.push(Problem { name, message }),
            }
        }
    }
    let mut digest = Sha256::new();
    for file in &files {
        digest.update(file.name.as_bytes());
        digest.update([0]);
        digest.update(file.text.as_bytes());
        digest.update([0]);
    }
    for problem in &problems {
        digest.update(problem.name.as_bytes());
        digest.update([1]);
        digest.update(problem.message.as_bytes());
        digest.update([1]);
    }
    let revision = digest
        .finalize()
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(Listing {
        revision,
        files,
        problems,
    })
}

/// One entry's text: a regular file reached without following links, bounded, UTF-8.
fn read_entry(directory: &Dir, name: &str) -> Result<String, String> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = directory
        .open_with(name, &options)
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("not a regular file".into());
    }
    if metadata.len() > max_file_bytes() {
        return Err(format!("larger than {} KiB", max_file_bytes() / 1024));
    }
    let mut bytes = Vec::new();
    file.take(max_file_bytes() + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > max_file_bytes() {
        return Err(format!("larger than {} KiB", max_file_bytes() / 1024));
    }
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text".into())
}

/// `GET /presentations[?after=<revision>]`.
pub(super) async fn read(State(shared): State<Shared>, request: Request) -> Response {
    let Some(path) = shared.services.presentations.clone() else {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    };
    let after = url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
        .find(|(key, _)| key == "after")
        .map(|(_, value)| value.into_owned());
    let listing = wait_for_change(&path, after.as_deref(), MAX_WAIT, &shared.stopped).await;
    match listing {
        Ok(listing) => (
            [
                (header::CONTENT_TYPE, "application/json"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            serde_json::to_string(&listing).unwrap_or_else(|_| "{}".into()),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("presentations: {error}"),
        )
            .into_response(),
    }
}

/// The listing once its revision differs from `after`, or after `wait`, or at shutdown.
pub(super) async fn wait_for_change(
    path: &Path,
    after: Option<&str>,
    wait: Duration,
    stopped: &CancellationToken,
) -> io::Result<Listing> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let owned = path.to_path_buf();
        let listing = tokio::task::spawn_blocking(move || read_listing(&owned))
            .await
            .map_err(io::Error::other)??;
        if after.is_none_or(|revision| revision != listing.revision)
            || tokio::time::Instant::now() >= deadline
        {
            return Ok(listing);
        }
        tokio::select! {
            _ = stopped.cancelled() => return Ok(listing),
            _ = tokio::time::sleep(POLL) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /* Synthetic data homes only, created in temporary directories. */

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().expect("temporary data home")
    }

    #[test]
    fn should_serve_yaml_files_verbatim_in_name_order_when_the_directory_has_entries() {
        // Arrange
        let home = home();
        let dir = home.path().join("presentations");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("b.yaml"),
            "version: 1\ntype: B\npresent: {kind: sankey}\n",
        )
        .unwrap();
        std::fs::write(dir.join("a.yml"), "version: 1\ntype: A\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        std::fs::write(dir.join(".hidden.yaml"), "ignored").unwrap();
        // Act
        let listing = read_listing(&dir).unwrap();
        // Assert: names sorted, other files ignored, text untouched (an unknown kind is not ours to judge).
        assert_eq!(
            listing
                .files
                .iter()
                .map(|file| file.name.as_str())
                .collect::<Vec<_>>(),
            ["a.yml", "b.yaml"]
        );
        assert_eq!(
            listing.files[1].text,
            "version: 1\ntype: B\npresent: {kind: sankey}\n"
        );
        assert!(listing.problems.is_empty());
    }

    #[test]
    fn should_answer_an_empty_listing_when_the_directory_does_not_exist() {
        let home = home();
        let listing = read_listing(&home.path().join("presentations")).unwrap();
        assert!(listing.files.is_empty());
        assert!(listing.problems.is_empty());
    }

    #[test]
    fn should_name_oversized_and_non_utf8_files_when_they_cannot_be_served() {
        let home = home();
        let dir = home.path().join("presentations");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("big.yaml"),
            vec![b'a'; (max_file_bytes() + 1) as usize],
        )
        .unwrap();
        std::fs::write(dir.join("binary.yaml"), [0xff, 0xfe, 0x00]).unwrap();
        let listing = read_listing(&dir).unwrap();
        assert!(listing.files.is_empty());
        assert_eq!(listing.problems.len(), 2);
        assert!(
            listing
                .problems
                .iter()
                .any(|problem| problem.name == "big.yaml" && problem.message.contains("KiB"))
        );
        assert!(
            listing
                .problems
                .iter()
                .any(|problem| problem.name == "binary.yaml" && problem.message.contains("UTF-8"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn should_not_follow_a_link_when_an_entry_points_outside_the_directory() {
        let home = home();
        let dir = home.path().join("presentations");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(home.path().join("secret.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(home.path().join("secret.txt"), dir.join("link.yaml")).unwrap();
        let listing = read_listing(&dir).unwrap();
        assert!(listing.files.is_empty());
        assert_eq!(listing.problems[0].name, "link.yaml");
    }

    #[test]
    fn should_change_the_revision_when_a_file_changes() {
        let home = home();
        let dir = home.path().join("presentations");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("a.yaml"), "version: 1\n").unwrap();
        let before = read_listing(&dir).unwrap().revision;
        assert_eq!(read_listing(&dir).unwrap().revision, before);
        std::fs::write(dir.join("a.yaml"), "version: 2\n").unwrap();
        assert_ne!(read_listing(&dir).unwrap().revision, before);
    }

    #[tokio::test]
    async fn should_answer_the_long_poll_with_the_change_when_the_directory_changes_while_waiting()
    {
        // Arrange
        let home = home();
        let dir = home.path().join("presentations");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("a.yaml"), "version: 1\n").unwrap();
        let before = read_listing(&dir).unwrap().revision;
        let stopped = CancellationToken::new();
        let writer = dir.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            std::fs::write(writer.join("b.yaml"), "version: 1\n").unwrap();
        });
        // Act
        let changed = wait_for_change(&dir, Some(&before), Duration::from_secs(10), &stopped)
            .await
            .unwrap();
        // Assert
        assert_ne!(changed.revision, before);
        assert_eq!(changed.files.len(), 2);
    }

    #[tokio::test]
    async fn should_answer_unchanged_when_the_wait_passes_without_a_change() {
        let home = home();
        let dir = home.path().join("presentations");
        let before = read_listing(&dir).unwrap().revision;
        let unchanged = wait_for_change(
            &dir,
            Some(&before),
            Duration::from_millis(50),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(unchanged.revision, before);
    }
}
