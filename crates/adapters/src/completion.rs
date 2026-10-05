//! Approximate shell word completion with bounded local metadata reads.
//! This is not shell parsing, expansion or execution. Quoted/escaped words may have no candidates.
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use wes_engine::{
    completion::{Candidate, Completer, CompletionError, Suggestions},
    driver::CancellationToken,
};
const MOST: usize = 60;
const BUILTINS: &[&str] = &[
    "cd", "echo", "exit", "export", "read", "set", "test", "unset", "eval", "exec", "shift",
    "trap", "umask", "wait", "true", "false", "pwd", "command", "printf",
];
pub struct ShellCompleter {
    base: PathBuf,
    home: Option<PathBuf>,
    path: Vec<PathBuf>,
}
impl ShellCompleter {
    /// Base/home/PATH are explicit startup configuration, while directory contents are read fresh.
    pub fn new(
        base: PathBuf,
        home: Option<PathBuf>,
        path: Vec<PathBuf>,
    ) -> Result<Self, CompletionError> {
        if !base.is_absolute() || home.as_ref().is_some_and(|p| !p.is_absolute()) {
            return Err(CompletionError::Invalid);
        }
        let mut bytes = base.as_os_str().len() + home.as_ref().map_or(0, |p| p.as_os_str().len());
        for directory in &path {
            bytes = bytes.saturating_add(directory.as_os_str().len());
        }
        if path.len() > 256 || bytes > 64 * 1024 {
            return Err(CompletionError::Capacity);
        }
        Ok(Self { base, home, path })
    }
    fn paths(&self, word: &str, work: &mut Work<'_>) -> Result<Vec<Candidate>, CompletionError> {
        let cut = word.rfind('/').map_or(0, |n| n + 1);
        let typed = &word[..cut];
        let leaf = &word[cut..];
        let directory = if let Some(relative) = typed.strip_prefix("~/") {
            let Some(home) = &self.home else {
                return Ok(vec![]);
            };
            home.join(relative)
        } else if Path::new(typed).is_absolute() {
            typed.into()
        } else {
            self.base.join(typed)
        };
        let mut found = BTreeSet::new();
        work.list(&directory, |entry| {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                return;
            };
            if !name.starts_with(leaf)
                || (leaf.is_empty() && name.starts_with('.'))
                || name.chars().any(char::is_control)
            {
                return;
            }
            let is_dir = entry.path().is_dir();
            let text = format!("{typed}{name}{}", if is_dir { "/" } else { "" });
            retain(
                &mut found,
                (text.encode_utf16().collect::<Vec<_>>(), text, is_dir),
            );
        })?;
        Ok(found
            .into_iter()
            .map(|(_, text, directory)| Candidate {
                text: quote(&text, typed.starts_with("~/")),
                kind: if directory { "directory" } else { "file" }.into(),
                detail: String::new(),
            })
            .collect())
    }
    fn programs(&self, word: &str, work: &mut Work<'_>) -> Result<Vec<Candidate>, CompletionError> {
        let mut found = Vec::new();
        let mut seen = BTreeSet::new();
        let mut builtin: Vec<_> = BUILTINS
            .iter()
            .filter(|name| name.starts_with(word))
            .copied()
            .collect();
        builtin.sort();
        for name in builtin {
            seen.insert(name.to_owned());
            found.push(name.to_owned());
        }
        for directory in &self.path {
            work.check()?;
            if found.len() >= MOST {
                break;
            }
            if directory.as_os_str().is_empty() {
                continue;
            }
            let directory = if directory.is_absolute() {
                directory.clone()
            } else {
                self.base.join(directory)
            };
            let mut names = BTreeSet::new();
            work.list(&directory, |entry| {
                let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                    return;
                };
                if !name.starts_with(word)
                    || name.chars().any(char::is_control)
                    || seen.contains(&name)
                    || !executable(&entry.path())
                {
                    return;
                }
                retain(&mut names, (name.encode_utf16().collect::<Vec<_>>(), name));
            })?;
            for (_, name) in names {
                if found.len() == MOST {
                    break;
                }
                seen.insert(name.clone());
                found.push(name);
            }
        }
        Ok(found
            .into_iter()
            .map(|text| Candidate {
                text: quote(&text, false),
                kind: "program".into(),
                detail: String::new(),
            })
            .collect())
    }
}
impl Completer for ShellCompleter {
    fn complete(
        &self,
        written: &str,
        caret: usize,
        cancelled: &CancellationToken,
    ) -> Result<Suggestions, CompletionError> {
        if written.len() > 16 * 1024 {
            return Err(CompletionError::Capacity);
        }
        let prefix = written.get(..caret).ok_or(CompletionError::Invalid)?;
        if written.contains('\0') {
            return Err(CompletionError::Invalid);
        }
        let mut work = Work {
            remaining: 20_000,
            cancelled,
        };
        work.check()?;
        let from = prefix
            .char_indices()
            .filter(|(_, c)| " \t|;&()=".contains(*c))
            .map(|(i, c)| i + c.len_utf8())
            .next_back()
            .unwrap_or(0);
        let word = &prefix[from..];
        let first = prefix[..from]
            .chars()
            .rev()
            .find(|c| *c != ' ' && *c != '\t')
            .is_none_or(|c| "|;&(".contains(c));
        let items = if first && !word.contains('/') {
            self.programs(word, &mut work)?
        } else {
            self.paths(word, &mut work)?
        };
        Ok(if items.is_empty() {
            Suggestions::default()
        } else {
            Suggestions { from, items }
        })
    }
}
struct Work<'a> {
    remaining: usize,
    cancelled: &'a CancellationToken,
}
impl Work<'_> {
    fn check(&self) -> Result<(), CompletionError> {
        if self.cancelled.is_cancelled() {
            Err(CompletionError::Cancelled)
        } else {
            Ok(())
        }
    }
    fn list(
        &mut self,
        path: &Path,
        mut visit: impl FnMut(std::fs::DirEntry),
    ) -> Result<(), CompletionError> {
        self.check()?;
        let Ok(entries) = std::fs::read_dir(path) else {
            return Ok(());
        };
        for entry in entries {
            self.check()?;
            self.remaining = self
                .remaining
                .checked_sub(1)
                .ok_or(CompletionError::Capacity)?;
            if let Ok(entry) = entry {
                visit(entry);
            }
        }
        Ok(())
    }
}
fn retain<T: Ord>(items: &mut BTreeSet<T>, item: T) {
    items.insert(item);
    if items.len() > MOST {
        items.pop_last();
    }
}
fn executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use rustix::fs::{Access, AtFlags, CWD, accessat};
        accessat(CWD, path, Access::EXEC_OK, AtFlags::EACCESS).is_ok()
    }
    #[cfg(not(unix))]
    {
        false
    }
}
/// Quote shell metacharacters, preserving home expansion only when explicitly typed.
fn quote(text: &str, expand_home: bool) -> String {
    let (prefix, text) = if expand_home {
        text.strip_prefix("~/")
            .map_or(("", text), |rest| ("~/", rest))
    } else {
        ("", text)
    };
    if text
        .chars()
        .all(|c| c.is_alphanumeric() || "_./-".contains(c))
    {
        return format!("{prefix}{text}");
    }
    format!("{prefix}'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_budget_refuses_a_partial_answer() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a"), []).unwrap();
        std::fs::write(root.path().join("b"), []).unwrap();
        let token = CancellationToken::new();
        let mut work = Work {
            remaining: 1,
            cancelled: &token,
        };
        assert!(matches!(
            work.list(root.path(), |_| {}),
            Err(CompletionError::Capacity)
        ));
    }
}
