//! Bounded streaming journal reads. Invalid records stop iteration; they are never silently skipped.
mod pages;
mod requests;
use crate::codec::{
    CodecError, Limits,
    history::{DecodedRecord, decode_journal, decode_recovery},
};
pub use crate::filesystem::Durability;
use crate::{
    codec::history::{encode_journal, encode_recovery},
    filesystem::{DirectoryKind, OwnedDirectory, private_options},
};
use std::{
    io::{self, BufRead, BufReader, Write},
    path::Path,
};
use thiserror::Error;
use wes_engine::history::{
    AppendReceipt, HistoryCapture, HistoryCaptureLimits, HistoryCheckpoint, HistoryImage,
    JournalEntry, JournalSink, Persistence, Record, RecordError, RecoveryEntry,
};

#[derive(Clone, Copy, Debug)]
pub struct ReadLimits {
    pub record: Limits,
    pub bytes: u64,
    /// Physical lines, including blank lines, so padding cannot bypass the work limit.
    pub lines: u64,
}
impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            record: Limits {
                // A 1MiB captured document can appear in input identity and snapshot evidence.
                // Match the recorder credit ceiling, including worst-case JSON escaping.
                bytes: wes_budgets::get("history.record.bytes") as usize,
                nodes: wes_budgets::get("history.record.nodes") as usize,
            },
            bytes: wes_budgets::get("history.read.bytes") as u64,
            lines: wes_budgets::get("history.read.lines") as u64,
        }
    }
}

#[derive(Debug)]
pub struct LocatedRecord<T> {
    pub entry: T,
    pub line: u64,
    pub offset: u64,
    pub end_offset: u64,
    /// A complete final JSON object is readable without a delimiter, but not append-ready.
    pub terminated: bool,
}

#[derive(Debug, Error)]
pub enum ReadError {
    #[error("journal read failed on line {line} at byte {offset}")]
    Io {
        line: u64,
        offset: u64,
        #[source]
        source: io::Error,
    },
    #[error("journal {budget} budget exceeded on line {line} at byte {offset}")]
    Limit {
        line: u64,
        offset: u64,
        budget: &'static str,
    },
    #[error("invalid journal record on line {line} at byte {offset}")]
    Record {
        line: u64,
        offset: u64,
        unterminated: bool,
        #[source]
        source: CodecError,
    },
}
impl ReadError {
    /// This identifies a syntactically incomplete final line, not arbitrary corruption at EOF.
    pub fn is_torn_tail(&self) -> bool {
        matches!(self, Self::Record { unterminated: true, source: CodecError::Json(error), .. } if error.is_eof())
    }
}

pub fn read_journal<R: BufRead>(reader: R, limits: ReadLimits) -> Records<R, JournalEntry> {
    Records::new(reader, limits, decode_journal)
}
pub fn read_recovery<R: BufRead>(reader: R, limits: ReadLimits) -> Records<R, RecoveryEntry> {
    Records::new(reader, limits, decode_recovery)
}

/// Retains at most one bounded line. The consumer controls whether/how many decoded records to keep.
pub struct Records<R, T> {
    reader: R,
    limits: ReadLimits,
    decode: fn(&[u8], Limits) -> Result<DecodedRecord<T>, CodecError>,
    line: u64,
    offset: u64,
    done: bool,
}
impl<R: BufRead, T> Records<R, T> {
    fn new(
        reader: R,
        limits: ReadLimits,
        decode: fn(&[u8], Limits) -> Result<DecodedRecord<T>, CodecError>,
    ) -> Self {
        Self {
            reader,
            limits,
            decode,
            line: 0,
            offset: 0,
            done: false,
        }
    }
    fn next_record(&mut self) -> Result<Option<LocatedRecord<T>>, ReadError> {
        loop {
            let line = self.line.saturating_add(1);
            let offset = self.offset;
            let mut bytes = Vec::new();
            let mut terminated = false;
            loop {
                let chunk = self.reader.fill_buf().map_err(|source| ReadError::Io {
                    line,
                    offset,
                    source,
                })?;
                if chunk.is_empty() {
                    break;
                }
                if line > self.limits.lines {
                    return Err(ReadError::Limit {
                        line,
                        offset,
                        budget: "line count",
                    });
                }
                let content = chunk
                    .iter()
                    .position(|b| *b == b'\n')
                    .unwrap_or(chunk.len());
                terminated = content < chunk.len();
                let consumed = content + usize::from(terminated);
                if consumed as u128 + u128::from(self.offset) > u128::from(self.limits.bytes) {
                    return Err(ReadError::Limit {
                        line,
                        offset,
                        budget: "total byte",
                    });
                }
                if content > self.limits.record.bytes.saturating_sub(bytes.len()) {
                    return Err(ReadError::Limit {
                        line,
                        offset,
                        budget: "record byte",
                    });
                }
                bytes.extend_from_slice(&chunk[..content]);
                self.reader.consume(consumed);
                self.offset += consumed as u64;
                if terminated {
                    break;
                }
            }
            if bytes.is_empty() && !terminated {
                return Ok(None);
            }
            self.line = line;
            // JSON whitespace only: non-JSON Unicode whitespace is not silently discarded.
            if bytes.iter().all(|b| matches!(b, b' ' | b'\t' | b'\r')) {
                continue;
            }
            let decoded =
                (self.decode)(&bytes, self.limits.record).map_err(|source| ReadError::Record {
                    line,
                    offset,
                    unterminated: !terminated,
                    source,
                })?;
            return Ok(Some(LocatedRecord {
                entry: decoded.entry,
                line,
                offset,
                end_offset: self.offset,
                terminated,
            }));
        }
    }
}
impl<R: BufRead, T> Iterator for Records<R, T> {
    type Item = Result<LocatedRecord<T>, ReadError>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_record() {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}
impl<R: BufRead, T> std::iter::FusedIterator for Records<R, T> {}

/// Journal and recovery share a writer lifetime but remain distinct files. No repair, truncation,
/// export or automatic forgetting occurs here. A failed append poisons this writer until reopened.
pub struct FileHistory {
    requests: requests::Requests,
    page_identity: uuid::Uuid,
    directory: OwnedDirectory,
    journal: Option<cap_std::fs::File>,
    recovery: Option<cap_std::fs::File>,
    journal_position: Position,
    recovery_position: Position,
    limits: ReadLimits,
    durability: Durability,
    poisoned: bool,
    protected_handles: std::collections::BTreeSet<wes_engine::storage::ValueHandle>,
    dataset_publication: Option<(
        String,
        std::sync::Arc<dyn wes_engine::history::RetainedDatasetPublication>,
    )>,
}
#[derive(Clone, Copy, Default)]
struct Position {
    offset: u64,
    lines: u64,
}
impl FileHistory {
    pub(crate) fn set_dataset_publication(
        &mut self,
        generation: &str,
        publication: Option<std::sync::Arc<dyn wes_engine::history::RetainedDatasetPublication>>,
    ) -> Result<(), RecordError> {
        let identity = uuid::Uuid::parse_str(generation)
            .map_err(|_| RecordError::Limit("workspace generation"))?;
        self.dataset_publication = publication.map(|port| (identity.to_string(), port));
        Ok(())
    }
    pub(crate) fn protect_image(&mut self, image: &HistoryImage) -> Result<(), RecordError> {
        if self.dataset_publication.is_none() {
            return Ok(());
        }
        let mut handles = std::collections::BTreeSet::new();
        for entry in image.journal() {
            handles.extend(
                entry
                    .retained_dataset_handles()
                    .map_err(|_| RecordError::Limit("retained history references"))?,
            );
            if handles.len() > 100_000 {
                return Err(RecordError::Limit("retained history references"));
            }
        }
        self.protect_handles(handles.into_iter().collect())
    }
    fn protect_handles(
        &mut self,
        handles: Vec<wes_engine::storage::ValueHandle>,
    ) -> Result<(), RecordError> {
        let Some((generation, publication)) = &self.dataset_publication else {
            return Ok(());
        };
        let new = handles
            .into_iter()
            .filter(|handle| !self.protected_handles.contains(handle))
            .collect::<std::collections::BTreeSet<_>>();
        if new.is_empty() {
            return Ok(());
        }
        if self.protected_handles.len().saturating_add(new.len()) > 100_000 {
            return Err(RecordError::Limit("retained history references"));
        }
        let receipt = publication.protect(generation, &new.iter().cloned().collect::<Vec<_>>())?;
        if receipt.iter().any(|handle| !new.contains(handle)) {
            return Err(RecordError::Limit("retained publication receipt"));
        }
        self.protected_handles.extend(receipt);
        Ok(())
    }
    /// Capture and sync both validated prefixes while exclusively owning this writer. This changes
    /// no journal contents and performs no repair/retry. Call on a joined blocking startup worker,
    /// or through Recorder.capture after moving this FileHistory into the recording worker.
    pub fn capture(&mut self, limits: HistoryCaptureLimits) -> Result<HistoryImage, RecordError> {
        self.capture_with_sync(limits, |history| {
            for file in [&history.journal, &history.recovery].into_iter().flatten() {
                file.sync_all()?;
            }
            history.directory.sync()
        })
    }
    fn capture_with_sync(
        &mut self,
        limits: HistoryCaptureLimits,
        sync: impl FnOnce(&Self) -> io::Result<()>,
    ) -> Result<HistoryImage, RecordError> {
        if self.poisoned {
            return Err(RecordError::Poisoned);
        }
        let mut capture = HistoryCapture::new(limits);
        let read = (|| {
            capture_file(
                self.journal.as_ref(),
                self.journal_position,
                self.limits,
                decode_journal,
                Record::Journal,
                &mut capture,
            )?;
            capture_file(
                self.recovery.as_ref(),
                self.recovery_position,
                self.limits,
                decode_recovery,
                Record::Recovery,
                &mut capture,
            )?;
            Ok(())
        })();
        if let Err(error) = read {
            // Capture capacity is a caller policy, not evidence that the owned stream changed.
            if !matches!(error, RecordError::Limit(_)) {
                self.poisoned = true;
            }
            return Err(error);
        }
        self.poisoned = true;
        sync(self).map_err(|error| {
            RecordError::backend("synchronizing captured history", false, error)
        })?;
        self.poisoned = false;
        let persistence = match self.durability {
            Durability::File => Persistence::FileSynced,
            Durability::FileAndDirectory => Persistence::FileAndDirectorySynced,
        };
        Ok(capture.finish(HistoryCheckpoint {
            journal: AppendReceipt {
                persistence,
                end_offset: self.journal_position.offset,
            },
            recovery: AppendReceipt {
                persistence,
                end_offset: self.recovery_position.offset,
            },
        }))
    }
    pub fn open(
        path: &Path,
        limits: ReadLimits,
        durability: Durability,
    ) -> Result<Self, RecordError> {
        let directory = OwnedDirectory::open(path, DirectoryKind::History, durability)
            .map_err(|e| RecordError::backend("opening the history directory", false, e))?;
        Self::from_directory(directory, limits, durability)
    }
    pub(crate) fn from_directory(
        directory: OwnedDirectory,
        limits: ReadLimits,
        durability: Durability,
    ) -> Result<Self, RecordError> {
        let journal = existing(&directory, "journal.jsonl")?;
        let recovery = existing(&directory, "recovery.jsonl")?;
        let mut requests = requests::Requests::default();
        let journal_position = inspect_with(journal.as_ref(), limits, decode_journal, |entry| {
            if let JournalEntry::Requested(record) = entry {
                requests.restore(record.clone())?;
            }
            Ok(())
        })?;
        let recovery_position = inspect(recovery.as_ref(), limits, decode_recovery)?;
        let history = Self {
            requests,
            directory,
            page_identity: uuid::Uuid::new_v4(),
            journal,
            recovery,
            journal_position,
            recovery_position,
            limits,
            durability,
            poisoned: false,
            dataset_publication: None,
            protected_handles: Default::default(),
        };
        Ok(history)
    }
    /// Seed an unpublished empty generation. Failure poisons it; callers must not publish it.
    /// Records use the current journal/recovery codecs; values remain handles.
    pub(crate) fn seed(&mut self, image: &HistoryImage) -> Result<(), RecordError> {
        if self.poisoned || self.journal.is_some() || self.recovery.is_some() {
            return Err(RecordError::Poisoned);
        }
        let maximum = HistoryCaptureLimits::default();
        if image.charged_bytes() > maximum.bytes
            || image.journal().len().saturating_add(image.recovery().len()) > maximum.records
        {
            return Err(RecordError::Limit("workspace image"));
        }
        self.protect_image(image)?;
        self.poisoned = true;
        let journal = seed_stream(
            &self.directory,
            "journal.jsonl",
            self.limits,
            image
                .journal()
                .iter()
                .map(|entry| crate::codec::history::encode_journal(entry, self.limits.record)),
        )?;
        let recovery = seed_stream(
            &self.directory,
            "recovery.jsonl",
            self.limits,
            image
                .recovery()
                .iter()
                .map(|entry| crate::codec::history::encode_recovery(entry, self.limits.record)),
        )?;
        self.directory
            .sync()
            .map_err(|e| RecordError::backend("synchronizing a workspace generation", false, e))?;
        (self.journal, self.journal_position) = journal;
        (self.recovery, self.recovery_position) = recovery;
        self.requests = requests::Requests::default();
        for entry in image.journal() {
            if let JournalEntry::Requested(record) = entry {
                self.requests.restore(record.clone())?;
            }
        }
        self.poisoned = false;
        Ok(())
    }
    /// Each reader uses a freshly opened file description, so its cursor is independent of writes.
    /// The borrow prevents appends until the read iterator is dropped.
    pub fn journal(
        &self,
    ) -> Result<
        impl Iterator<Item = Result<LocatedRecord<JournalEntry>, ReadError>> + '_,
        RecordError,
    > {
        Ok(self
            .reader("journal.jsonl")?
            .into_iter()
            .flat_map(|file| read_journal(BufReader::new(file), self.limits)))
    }
    pub fn recovery(
        &self,
    ) -> Result<
        impl Iterator<Item = Result<LocatedRecord<RecoveryEntry>, ReadError>> + '_,
        RecordError,
    > {
        Ok(self
            .reader("recovery.jsonl")?
            .into_iter()
            .flat_map(|file| read_recovery(BufReader::new(file), self.limits)))
    }
    fn reader(&self, name: &str) -> Result<Option<cap_std::fs::File>, RecordError> {
        let mut options = private_options();
        options.read(true);
        let file = match self.directory.open_with(name, &options) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(RecordError::backend(
                    "opening a history reader",
                    false,
                    error,
                ));
            }
        };
        if !file
            .metadata()
            .map_err(|e| RecordError::backend("checking a history reader", false, e))?
            .is_file()
        {
            return Err(RecordError::backend(
                "checking a history reader",
                false,
                io::Error::other("not a regular file"),
            ));
        }
        Ok(Some(file))
    }
}
impl JournalSink for FileHistory {
    fn claim_request(
        &mut self,
        record: wes_engine::history::RequestRecord,
        current: bool,
    ) -> Result<wes_engine::history::RequestClaim, RecordError> {
        self.reserve_request(record, current)
    }
    fn find_request(
        &mut self,
        actor: &str,
        request: &str,
    ) -> Result<Option<wes_engine::history::RequestRecord>, RecordError> {
        self.lookup_request(actor, request)
    }
    fn page(
        &mut self,
        cursor: Option<wes_engine::history::HistoryCursor>,
        limits: wes_engine::history::HistoryPageLimits,
    ) -> Result<wes_engine::history::HistoryPage, RecordError> {
        self.read_page(cursor, limits)
    }
    fn capture(&mut self, limits: HistoryCaptureLimits) -> Result<HistoryImage, RecordError> {
        FileHistory::capture(self, limits)
    }
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        self.append_record(record, |file, directory| {
            file.sync_all()?;
            directory.sync()
        })
    }
}
impl FileHistory {
    fn append_record(
        &mut self,
        record: &Record,
        sync: impl FnOnce(&cap_std::fs::File, &OwnedDirectory) -> io::Result<()>,
    ) -> Result<AppendReceipt, RecordError> {
        if self.poisoned {
            return Err(RecordError::Poisoned);
        }
        if let Record::Journal(JournalEntry::Requested(request)) = record {
            request
                .validate()
                .map_err(|_| RecordError::Limit("request identity"))?;
            if self
                .lookup_request(&request.namespace, &request.request)?
                .is_some()
            {
                return Err(RecordError::RequestConflict);
            }
        }
        let (encoded, current, name) = match record {
            Record::Journal(entry) => (
                encode_journal(entry, self.limits.record),
                self.journal_position,
                "journal.jsonl",
            ),
            Record::Recovery(entry) => (
                encode_recovery(entry, self.limits.record),
                self.recovery_position,
                "recovery.jsonl",
            ),
        };
        let bytes =
            encoded.map_err(|e| RecordError::backend("encoding a history record", false, e))?;
        let end = u128::from(current.offset) + bytes.len() as u128 + 1;
        if end > u128::from(self.limits.bytes) {
            return Err(RecordError::Limit("total byte"));
        }
        if current.lines >= self.limits.lines {
            return Err(RecordError::Limit("line count"));
        }
        if let Record::Journal(entry) = record {
            let handles = entry
                .retained_dataset_handles()
                .map_err(|_| RecordError::Limit("retained history references"))?;
            self.protect_handles(handles)?;
        }
        let (file, position) = match record {
            Record::Journal(_) => (&mut self.journal, &mut self.journal_position),
            Record::Recovery(_) => (&mut self.recovery, &mut self.recovery_position),
        };
        if file.is_none() {
            let mut options = private_options();
            options.read(true).append(true).create_new(true);
            *file = Some(
                self.directory
                    .open_with(name, &options)
                    .map_err(|e| RecordError::backend("creating a history file", false, e))?,
            );
        }
        let writer = file.as_mut().expect("opened above");
        // Set before entering fallible I/O. No later append can concatenate onto a torn line.
        self.poisoned = true;
        append_line(writer, &bytes, |file| sync(file, &self.directory))
            .map_err(|e| RecordError::backend("appending and syncing history", true, e))?;
        *position = Position {
            offset: end as u64,
            lines: position.lines + 1,
        };
        self.poisoned = false;
        let end_offset = position.offset;
        self.remember_request(record);
        Ok(AppendReceipt {
            persistence: match self.durability {
                Durability::File => Persistence::FileSynced,
                Durability::FileAndDirectory => Persistence::FileAndDirectorySynced,
            },
            end_offset,
        })
    }
}
fn seed_stream(
    directory: &OwnedDirectory,
    name: &str,
    limits: ReadLimits,
    records: impl Iterator<Item = Result<Vec<u8>, CodecError>>,
) -> Result<(Option<cap_std::fs::File>, Position), RecordError> {
    let mut options = private_options();
    options.read(true).append(true).create_new(true);
    let mut file = directory
        .open_with(name, &options)
        .map_err(|e| RecordError::backend("creating a workspace stream", false, e))?;
    let mut position = Position::default();
    for bytes in records {
        let bytes =
            bytes.map_err(|e| RecordError::backend("copying a workspace record", false, e))?;
        let end = position
            .offset
            .saturating_add(bytes.len() as u64)
            .saturating_add(1);
        if end > limits.bytes || position.lines >= limits.lines {
            return Err(RecordError::Limit("workspace stream"));
        }
        file.write_all(&bytes)
            .and_then(|()| file.write_all(b"\n"))
            .map_err(|e| RecordError::backend("writing a workspace stream", false, e))?;
        position = Position {
            offset: end,
            lines: position.lines + 1,
        };
    }
    file.sync_all()
        .map_err(|e| RecordError::backend("synchronizing a workspace stream", false, e))?;
    Ok((Some(file), position))
}

fn existing(
    directory: &OwnedDirectory,
    name: &str,
) -> Result<Option<cap_std::fs::File>, RecordError> {
    let mut options = private_options();
    options.read(true).append(true);
    let file = match directory.open_with(name, &options) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(RecordError::backend(
                "opening an existing history file",
                false,
                e,
            ));
        }
    };
    if !file
        .metadata()
        .map_err(|e| RecordError::backend("checking a history file", false, e))?
        .is_file()
    {
        return Err(RecordError::backend(
            "checking a history file",
            false,
            io::Error::other("not a regular file"),
        ));
    }
    Ok(Some(file))
}
fn capture_file<T>(
    file: Option<&cap_std::fs::File>,
    expected: Position,
    limits: ReadLimits,
    decode: fn(&[u8], Limits) -> Result<DecodedRecord<T>, CodecError>,
    wrap: fn(T) -> Record,
    capture: &mut HistoryCapture,
) -> Result<(), RecordError> {
    use std::io::{Seek, SeekFrom};
    let Some(mut file) = file else {
        return Ok(());
    };
    file.seek(SeekFrom::Start(0))
        .map_err(|error| RecordError::backend("seeking captured history", false, error))?;
    let mut records = Records::new(BufReader::new(file), limits, decode);
    for record in records.by_ref() {
        let record = record
            .map_err(|error| RecordError::backend("reading captured history", false, error))?;
        if !record.terminated {
            return Err(RecordError::backend(
                "validating captured history",
                false,
                io::Error::other("unterminated captured history"),
            ));
        }
        capture.push(wrap(record.entry))?;
    }
    if records.offset != expected.offset || records.line != expected.lines {
        return Err(RecordError::backend(
            "validating captured history",
            false,
            io::Error::other("captured history changed outside its writer"),
        ));
    }
    Ok(())
}
fn inspect<T>(
    file: Option<&cap_std::fs::File>,
    limits: ReadLimits,
    decode: fn(&[u8], Limits) -> Result<DecodedRecord<T>, CodecError>,
) -> Result<Position, RecordError> {
    inspect_with(file, limits, decode, |_| Ok(()))
}
fn inspect_with<T>(
    file: Option<&cap_std::fs::File>,
    limits: ReadLimits,
    decode: fn(&[u8], Limits) -> Result<DecodedRecord<T>, CodecError>,
    mut visit: impl FnMut(&T) -> Result<(), RecordError>,
) -> Result<Position, RecordError> {
    let Some(file) = file else {
        return Ok(Position::default());
    };
    let mut records = Records::new(BufReader::new(file), limits, decode);
    for entry in records.by_ref() {
        let entry =
            entry.map_err(|e| RecordError::backend("validating existing history", false, e))?;
        visit(&entry.entry)?;
        if !entry.terminated {
            return Err(RecordError::backend(
                "validating existing history",
                false,
                io::Error::other(
                    "history is missing its final newline; explicit recovery is required",
                ),
            ));
        }
    }
    // A whitespace-only trailing fragment is also not append-ready.
    let offset = records.offset;
    if offset != 0 {
        use std::io::{Read, Seek, SeekFrom};
        let mut file = file;
        file.seek(SeekFrom::End(-1))
            .and_then(|_| {
                let mut byte = [0];
                file.read_exact(&mut byte)?;
                if byte[0] == b'\n' {
                    Ok(())
                } else {
                    Err(io::Error::other("history has an unterminated tail"))
                }
            })
            .map_err(|e| RecordError::backend("checking the history boundary", false, e))?;
    }
    Ok(Position {
        offset,
        lines: records.line,
    })
}
fn append_line<W: Write>(
    writer: &mut W,
    bytes: &[u8],
    sync: impl FnOnce(&W) -> io::Result<()>,
) -> io::Result<()> {
    writer.write_all(bytes)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    sync(writer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, fs};

    #[test]
    fn failed_capture_sync_returns_no_checkpoint_and_requires_reopen_without_content_changes() {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o700));
        }
        let dir = builder.tempdir().unwrap();
        let mut history =
            FileHistory::open(dir.path(), ReadLimits::default(), Durability::File).unwrap();
        let entry = Record::Recovery(RecoveryEntry::Accepted {
            cell: "old-cell".into(),
        });
        history.append(&entry).unwrap();
        let before = fs::read(dir.path().join("recovery.jsonl")).unwrap();
        let result = history.capture_with_sync(HistoryCaptureLimits::default(), |_| {
            Err(io::Error::other("synthetic private sync failure"))
        });
        assert!(matches!(
            result,
            Err(RecordError::Backend {
                may_have_appended: false,
                ..
            })
        ));
        assert!(matches!(history.append(&entry), Err(RecordError::Poisoned)));
        assert_eq!(fs::read(dir.path().join("recovery.jsonl")).unwrap(), before);
        drop(history);
        let mut history =
            FileHistory::open(dir.path(), ReadLimits::default(), Durability::File).unwrap();
        assert_eq!(
            history
                .capture(HistoryCaptureLimits::default())
                .unwrap()
                .recovery()
                .len(),
            1
        );
    }

    struct Writer {
        bytes: Vec<u8>,
        fail_after: Option<usize>,
        flushed: bool,
    }
    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail_after.is_some_and(|at| self.bytes.len() >= at) {
                return Err(io::Error::other("injected write failure"));
            }
            let n = bytes.len().min(1);
            self.bytes.extend(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushed = true;
            Ok(())
        }
    }
    #[test]
    fn incomplete_record_or_delimiter_never_reaches_sync() {
        for fail_after in 0..=3 {
            let mut writer = Writer {
                bytes: vec![],
                fail_after: Some(fail_after),
                flushed: false,
            };
            assert!(
                append_line(&mut writer, b"abc", |_| panic!(
                    "incomplete line must not sync"
                ))
                .is_err()
            );
            assert_eq!(writer.bytes.len(), fail_after);
            assert!(!writer.flushed);
        }
    }
    #[test]
    fn short_writes_flush_the_complete_line_before_sync_acknowledgement() {
        let mut writer = Writer {
            bytes: vec![],
            fail_after: None,
            flushed: false,
        };
        let synced = Cell::new(false);
        append_line(&mut writer, b"abc", |writer| {
            assert_eq!(writer.bytes, b"abc\n");
            assert!(writer.flushed);
            synced.set(true);
            Ok(())
        })
        .unwrap();
        assert!(synced.get());
    }
    #[test]
    fn a_failed_sync_is_unconfirmed_not_rolled_back_and_poison_blocks_both_streams() {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o700));
        }
        let directory = builder.tempdir().unwrap();
        let mut history =
            FileHistory::open(directory.path(), ReadLimits::default(), Durability::File).unwrap();
        let record = Record::Recovery(RecoveryEntry::Accepted {
            cell: "cell".into(),
        });
        let error = history
            .append_record(&record, |_, _| {
                Err(io::Error::other("injected sync failure"))
            })
            .unwrap_err();
        assert!(matches!(
            error,
            RecordError::Backend {
                may_have_appended: true,
                ..
            }
        ));
        assert!(matches!(
            history.append(&record),
            Err(RecordError::Poisoned)
        ));
        let command = Record::Journal(JournalEntry::Command(wes_engine::history::CommandRecord {
            source_name: "fixture.wes".into(),
            source_start: wes_language::Position { line: 1, column: 1 },
            changed_nodes: vec![],
            document: None,
            revision_of: None,
            environments: None,
            cell: "cell".into(),
            text: "catalog list".into(),
            replay: "catalog list".into(),
            nodes: vec![],
            type_sources: Default::default(),
            calculation_package: None,
            imports: vec![],
        }));
        assert!(matches!(
            history.append(&command),
            Err(RecordError::Poisoned)
        ));
        assert!(!directory.path().join("journal.jsonl").exists());
        drop(history);
        let restored =
            FileHistory::open(directory.path(), ReadLimits::default(), Durability::File).unwrap();
        // Valid complete bytes may survive even though their previous acknowledgement failed.
        assert_eq!(restored.recovery().unwrap().count(), 1);
    }
}
