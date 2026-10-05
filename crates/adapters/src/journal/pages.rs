//! Seek within the already owned journal descriptor. No source execution, appends or pathname reads.
use super::*;
use std::io::{Read, Seek, SeekFrom};
use wes_engine::history::{HistoryCursor, HistoryPage, HistoryPageLimits};

fn max_scanned_records() -> usize {
    wes_budgets::get("history.scan.records") as usize
}
impl FileHistory {
    pub(super) fn read_page(
        &mut self,
        cursor: Option<HistoryCursor>,
        limits: HistoryPageLimits,
    ) -> Result<HistoryPage, RecordError> {
        limits.validate()?;
        if self.poisoned {
            return Err(RecordError::Poisoned);
        }
        let result = self.page_from_owned(cursor, limits);
        if matches!(result, Err(RecordError::Backend { .. })) {
            self.poisoned = true;
        }
        result
    }
    fn page_from_owned(
        &self,
        cursor: Option<HistoryCursor>,
        limits: HistoryPageLimits,
    ) -> Result<HistoryPage, RecordError> {
        let boundary = cursor.map_or(self.journal_position.offset, HistoryCursor::boundary);
        let offset = cursor.map_or(0, HistoryCursor::offset);
        if cursor.is_some_and(|cursor| cursor.journal() != self.page_identity)
            || offset > boundary
            || boundary > self.journal_position.offset
        {
            return Err(RecordError::InvalidCursor);
        }
        let persistence = match self.durability {
            Durability::File => Persistence::FileSynced,
            Durability::FileAndDirectory => Persistence::FileAndDirectorySynced,
        };
        let mut page = HistoryPage::new(AppendReceipt {
            persistence,
            end_offset: boundary,
        });
        let Some(mut file) = self.journal.as_ref() else {
            if boundary != 0 {
                return Err(RecordError::InvalidCursor);
            }
            self.directory
                .sync()
                .map_err(|error| failed("synchronizing empty history page", error))?;
            return Ok(page);
        };
        if file
            .metadata()
            .map_err(|error| failed("inspecting paged history", error))?
            .len()
            != self.journal_position.offset
        {
            return Err(failed(
                "validating paged history",
                io::Error::other("owned journal changed outside its writer"),
            ));
        }
        for at in [offset, boundary] {
            if at != 0 {
                file.seek(SeekFrom::Start(at - 1))
                    .map_err(|error| failed("seeking page boundary", error))?;
                let mut byte = [0];
                file.read_exact(&mut byte)
                    .map_err(|error| failed("reading page boundary", error))?;
                if byte != *b"\n" {
                    return Err(RecordError::InvalidCursor);
                }
            }
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| failed("seeking paged history", error))?;
        let reader = BufReader::new((&mut file).take(boundary - offset));
        let mut records = read_journal(
            reader,
            ReadLimits {
                bytes: boundary - offset,
                ..self.limits
            },
        );
        let mut next_offset = offset;
        for _ in 0..max_scanned_records() {
            let Some(record) = records.next() else {
                next_offset = boundary;
                break;
            };
            let record = record
                .map_err(|error| RecordError::backend("decoding paged history", false, error))?;
            if !record.terminated {
                return Err(failed(
                    "validating paged history",
                    io::Error::other("unterminated owned history record"),
                ));
            }
            if matches!(
                record.entry,
                JournalEntry::Observed(_)
                    | JournalEntry::Snapshot(_)
                    | JournalEntry::Diagnosed(_)
                    | JournalEntry::Noticed(_)
            ) && let Err(error) = page.push(record.entry, limits)
            {
                if page.entries().is_empty() {
                    return Err(error);
                }
                next_offset = offset + record.offset;
                break;
            }
            next_offset = offset + record.end_offset;
            if page.entries().len() == limits.entries {
                break;
            }
        }
        drop(records);
        file.sync_all()
            .map_err(|error| failed("synchronizing history page", error))?;
        self.directory
            .sync()
            .map_err(|error| failed("synchronizing history page directory", error))?;
        let next = (next_offset < boundary)
            .then(|| HistoryCursor::new(self.page_identity, boundary, next_offset))
            .transpose()?;
        Ok(page.with_next(next))
    }
}
fn failed(operation: &'static str, error: io::Error) -> RecordError {
    RecordError::backend(operation, false, error)
}
