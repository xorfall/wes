use super::schema::Record;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
pub fn segment_bytes() -> u64 {
    wes_budgets::get("diagnostics.segment.bytes") as u64
}
pub fn capture_bytes() -> u64 {
    wes_budgets::get("diagnostics.capture.bytes") as u64
}
pub fn max_export_bytes() -> u64 {
    wes_budgets::get("diagnostics.export.bytes") as u64
}
const NAMES: [&str; 6] = [
    "basic-0.jsonl",
    "basic-1.jsonl",
    "basic-2.jsonl",
    "basic-3.jsonl",
    "basic-4.jsonl",
    "capture.jsonl",
];

fn regular(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() && !m.file_type().is_symlink() => Ok(Some(m)),
        Ok(_) => Err(io::Error::other("Telemetry files must be ordinary files.")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}
fn open(path: &Path, truncate: bool) -> io::Result<File> {
    regular(path)?;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create(true)
        .truncate(truncate)
        .append(!truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    }
    options.open(path)
}
pub struct Files {
    root: PathBuf,
    index: usize,
    basic: Option<File>,
    basic_bytes: u64,
    capture: Option<File>,
    capture_bytes: u64,
}
impl Files {
    pub fn new(root: PathBuf) -> io::Result<Self> {
        for name in NAMES {
            regular(&root.join(name))?;
        }
        let bytes = regular(&root.join(NAMES[0]))?.map_or(0, |m| m.len());
        let mut files = Self {
            root,
            index: 0,
            basic: None,
            basic_bytes: bytes,
            capture: None,
            capture_bytes: 0,
        };
        // Reject oversized foreign/corrupt files instead of exporting or deleting them implicitly.
        for (i, name) in NAMES.iter().enumerate() {
            let maximum = if i == 5 {
                capture_bytes()
            } else {
                segment_bytes()
            };
            if regular(&files.root.join(name))?.is_some_and(|m| m.len() > maximum) {
                return Err(io::Error::other("Telemetry file exceeds its quota."));
            }
        }
        files.basic_bytes = bytes;
        Ok(files)
    }
    pub fn remove_expired_capture(&mut self) -> io::Result<()> {
        if let Some(meta) = regular(&self.root.join("capture.jsonl"))?
            && meta
                .modified()?
                .elapsed()
                .is_ok_and(|age| age.as_secs() >= 86_400)
        {
            fs::remove_file(self.root.join("capture.jsonl"))?;
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn test_fill_capture(&mut self) {
        self.capture_bytes = capture_bytes();
    }
    pub fn start_capture(&mut self) -> io::Result<()> {
        self.capture = Some(open(&self.root.join("capture.jsonl"), true)?);
        self.capture_bytes = 0;
        Ok(())
    }
    /// Returns false at the capture byte ceiling. One complete JSON object per line.
    pub fn write(&mut self, record: Record, capture: bool) -> io::Result<bool> {
        let mut bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
        bytes.push(b'\n');
        if bytes.len() > 2048 {
            return Err(io::Error::other(
                "Telemetry record exceeds its schema budget.",
            ));
        }
        if capture {
            if self.capture_bytes + bytes.len() as u64 > capture_bytes() {
                return Ok(false);
            }
            if let Some(file) = &mut self.capture {
                file.write_all(&bytes)?;
                self.capture_bytes += bytes.len() as u64;
            }
        } else {
            if self.basic_bytes + bytes.len() as u64 > segment_bytes() {
                self.index = (self.index + 1) % 5;
                self.basic = Some(open(&self.root.join(NAMES[self.index]), true)?);
                self.basic_bytes = 0;
            }
            if self.basic.is_none() {
                self.basic = Some(open(&self.root.join(NAMES[self.index]), false)?);
            }
            self.basic
                .as_mut()
                .expect("opened writer")
                .write_all(&bytes)?;
            self.basic_bytes += bytes.len() as u64;
        }
        Ok(true)
    }
    pub fn clear(&mut self) -> io::Result<()> {
        self.basic = None;
        self.capture = None;
        for name in NAMES {
            let path = self.root.join(name);
            if regular(&path)?.is_some() {
                fs::remove_file(path)?;
            }
        }
        self.basic_bytes = 0;
        self.capture_bytes = 0;
        self.index = 0;
        Ok(())
    }
    pub fn export(&self) -> io::Result<Vec<serde_json::Value>> {
        let mut logs = Vec::new();
        let mut total = 0;
        for name in NAMES {
            let path = self.root.join(name);
            let Some(meta) = regular(&path)? else {
                continue;
            };
            total += meta.len();
            if total > max_export_bytes() {
                return Err(io::Error::other("Telemetry export exceeds its limit."));
            }
            let mut text = String::new();
            File::open(path)?
                .take(max_export_bytes() + 1)
                .read_to_string(&mut text)?;
            // Revalidate saved output as a closed schema; never export arbitrary edited text.
            let mut safe = String::new();
            for line in text.lines() {
                if line.len() > 2048 {
                    return Err(io::Error::other("Invalid telemetry record."));
                }
                let record: Record = serde_json::from_str(line)
                    .map_err(|_| io::Error::other("Invalid telemetry record."))?;
                safe.push_str(&serde_json::to_string(&record).map_err(io::Error::other)?);
                safe.push('\n');
            }
            logs.push(serde_json::json!({"file":name,"jsonl":safe}));
        }
        Ok(logs)
    }
}
