//! Bounded, stable-prefix history reads. Cursor identity belongs to one concrete writer lifetime.
use super::{AppendReceipt, JournalEntry, Record, RecordError};
use std::{fmt, str::FromStr};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryCursor {
    journal: Uuid,
    boundary: u64,
    offset: u64,
}
impl HistoryCursor {
    pub fn new(journal: Uuid, boundary: u64, offset: u64) -> Result<Self, RecordError> {
        if offset > boundary {
            return Err(RecordError::InvalidCursor);
        }
        Ok(Self {
            journal,
            boundary,
            offset,
        })
    }
    pub fn journal(self) -> Uuid {
        self.journal
    }
    pub fn boundary(self) -> u64 {
        self.boundary
    }
    pub fn offset(self) -> u64 {
        self.offset
    }
}
impl fmt::Display for HistoryCursor {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}:{}:{}", self.journal, self.boundary, self.offset)
    }
}
impl FromStr for HistoryCursor {
    type Err = RecordError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if text.len() > 80 {
            return Err(RecordError::InvalidCursor);
        }
        let mut fields = text.split(':');
        let journal = fields
            .next()
            .and_then(|field| Uuid::parse_str(field).ok())
            .ok_or(RecordError::InvalidCursor)?;
        let boundary = fields
            .next()
            .and_then(|field| field.parse().ok())
            .ok_or(RecordError::InvalidCursor)?;
        let offset = fields
            .next()
            .and_then(|field| field.parse().ok())
            .ok_or(RecordError::InvalidCursor)?;
        let cursor = Self::new(journal, boundary, offset)?;
        if fields.next().is_some() || cursor.to_string() != text {
            return Err(RecordError::InvalidCursor);
        }
        Ok(cursor)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct HistoryPageLimits {
    pub entries: usize,
    /// Conservative retained entry charge; excludes one currently decoded physical record.
    pub bytes: u64,
}
impl Default for HistoryPageLimits {
    fn default() -> Self {
        Self {
            entries: wes_budgets::get("history.page.entries") as usize,
            bytes: wes_budgets::get("history.page.bytes") as u64,
        }
    }
}
impl HistoryPageLimits {
    pub fn validate(self) -> Result<(), RecordError> {
        if self.entries == 0
            || self.entries > 1000
            || self.bytes == 0
            || self.bytes > 32 * 1024 * 1024
        {
            return Err(RecordError::Limit("history page policy"));
        }
        Ok(())
    }
}
#[derive(Debug)]
pub struct HistoricalEntry {
    pub entry: JournalEntry,
}
#[derive(Debug)]
pub struct HistoryPage {
    entries: Vec<HistoricalEntry>,
    checkpoint: AppendReceipt,
    next: Option<HistoryCursor>,
    bytes: u64,
}
impl HistoryPage {
    pub fn new(checkpoint: AppendReceipt) -> Self {
        Self {
            entries: vec![],
            checkpoint,
            next: None,
            bytes: 0,
        }
    }
    pub fn entries(&self) -> &[HistoricalEntry] {
        &self.entries
    }
    pub fn checkpoint(&self) -> AppendReceipt {
        self.checkpoint
    }
    pub fn next(&self) -> Option<HistoryCursor> {
        self.next
    }
    pub fn charged_bytes(&self) -> u64 {
        self.bytes
    }
    pub fn push(
        &mut self,
        entry: JournalEntry,
        limits: HistoryPageLimits,
    ) -> Result<(), RecordError> {
        if !matches!(
            entry,
            JournalEntry::Observed(_)
                | JournalEntry::Snapshot(_)
                | JournalEntry::Diagnosed(_)
                | JournalEntry::Noticed(_)
        ) {
            return Err(RecordError::Limit("history page entry kind"));
        }
        let charge = crate::recording::record_charge(&Record::Journal(entry.clone()));
        if self.entries.len() >= limits.entries || charge > limits.bytes.saturating_sub(self.bytes)
        {
            return Err(RecordError::Limit("history page response"));
        }
        self.bytes += charge;
        self.entries.push(HistoricalEntry { entry });
        Ok(())
    }
    pub fn with_next(mut self, next: Option<HistoryCursor>) -> Self {
        self.next = next;
        self
    }
    pub(crate) fn validate(
        &self,
        cursor: Option<HistoryCursor>,
        limits: HistoryPageLimits,
    ) -> Result<(), RecordError> {
        if self.entries.len() > limits.entries || self.bytes > limits.bytes {
            return Err(RecordError::Limit("history page response"));
        }
        if let Some(cursor) = cursor
            && self.checkpoint.end_offset != cursor.boundary()
        {
            return Err(RecordError::InvalidCursor);
        }
        if let Some(next) = self.next
            && (next.boundary() != self.checkpoint.end_offset
                || next.offset() >= next.boundary()
                || next.offset() <= cursor.map_or(0, HistoryCursor::offset)
                || cursor.is_some_and(|cursor| cursor.journal() != next.journal()))
        {
            return Err(RecordError::InvalidCursor);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cursor_roundtrips_exact_offsets_and_rejects_noncanonical_or_nonprogressing_prefixes() {
        let id = Uuid::new_v4();
        let cursor = HistoryCursor::new(id, u64::MAX, 9007199254740993).unwrap();
        assert_eq!(cursor.to_string().parse::<HistoryCursor>().unwrap(), cursor);
        for text in [
            format!("{id}:10:11"),
            format!("{id}:010:0"),
            format!("{id}:10:+1"),
            format!("{id}:10:1:2"),
            "x".repeat(81),
        ] {
            assert!(matches!(
                text.parse::<HistoryCursor>(),
                Err(RecordError::InvalidCursor)
            ));
        }
        let page = HistoryPage::new(AppendReceipt {
            persistence: super::super::Persistence::FileSynced,
            end_offset: 10,
        })
        .with_next(Some(HistoryCursor::new(id, 10, 0).unwrap()));
        assert!(matches!(
            page.validate(None, HistoryPageLimits::default()),
            Err(RecordError::InvalidCursor)
        ));
    }
}
