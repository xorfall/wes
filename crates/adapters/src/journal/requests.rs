//! Bounded hot identities + no-false-negative membership filter. Journal is the authority.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Seek, SeekFrom};
use wes_engine::history::{RequestClaim, RequestRecord};

const HOT: usize = 10_000;
fn filter_bytes() -> usize {
    wes_budgets::get("history.filter.bytes") as usize
}
pub(super) struct Requests {
    hot: BTreeMap<(String, String), (RequestRecord, bool)>,
    order: VecDeque<(String, String)>,
    filter: Vec<u8>,
    #[cfg(test)]
    scans: std::cell::Cell<usize>,
}
impl Default for Requests {
    fn default() -> Self {
        Self {
            hot: BTreeMap::new(),
            order: VecDeque::new(),
            filter: vec![0; filter_bytes()],
            #[cfg(test)]
            scans: std::cell::Cell::new(0),
        }
    }
}
impl Requests {
    pub(super) fn restore(&mut self, record: RequestRecord) -> Result<(), RecordError> {
        if self
            .hot
            .get(&(record.namespace.clone(), record.request.clone()))
            .is_some_and(|(r, _)| r != &record)
        {
            return Err(RecordError::RequestConflict);
        }
        // A cold repeated key (or filter collision) is verified against the journal on lookup.
        let verified = !self.maybe(&record.namespace, &record.request);
        self.insert(record, verified);
        Ok(())
    }
    fn bits(namespace: &str, request: &str) -> [usize; 4] {
        let mut hash = Sha256::new();
        hash.update((namespace.len() as u64).to_le_bytes());
        hash.update(namespace);
        hash.update(request);
        let digest = hash.finalize();
        std::array::from_fn(|i| {
            u32::from_le_bytes(digest[i * 4..i * 4 + 4].try_into().unwrap()) as usize
                % (filter_bytes() * 8)
        })
    }
    fn maybe(&self, namespace: &str, request: &str) -> bool {
        Self::bits(namespace, request)
            .iter()
            .all(|bit| self.filter[bit / 8] & (1 << (bit % 8)) != 0)
    }
    fn remember(&mut self, record: RequestRecord) {
        self.insert(record, true);
    }
    fn insert(&mut self, record: RequestRecord, verified: bool) {
        for bit in Self::bits(&record.namespace, &record.request) {
            self.filter[bit / 8] |= 1 << (bit % 8);
        }
        let key = (record.namespace.clone(), record.request.clone());
        if !self.hot.contains_key(&key) {
            if self.hot.len() == HOT {
                self.hot
                    .remove(&self.order.pop_front().expect("hot index order"));
            }
            self.order.push_back(key.clone());
        }
        self.hot.insert(key, (record, verified));
    }
    fn get(&self, namespace: &str, request: &str) -> Option<RequestRecord> {
        self.hot
            .get(&(namespace.to_owned(), request.to_owned()))
            .filter(|(_, verified)| *verified)
            .map(|(r, _)| r.clone())
    }
}
impl FileHistory {
    fn scan_request(
        &self,
        namespace: &str,
        request: &str,
    ) -> Result<Option<RequestRecord>, RecordError> {
        #[cfg(test)]
        self.requests.scans.set(self.requests.scans.get() + 1);
        let Some(mut file) = self.journal.as_ref() else {
            return Ok(None);
        };
        if file
            .metadata()
            .map_err(|e| RecordError::backend("checking request history", false, e))?
            .len()
            != self.journal_position.offset
        {
            return Err(RecordError::Poisoned);
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|e| RecordError::backend("seeking request history", false, e))?;
        let records = read_journal(
            BufReader::new((&mut file).take(self.journal_position.offset)),
            self.limits,
        );
        let mut found = None;
        for item in records {
            let item =
                item.map_err(|e| RecordError::backend("reading request history", false, e))?;
            if let JournalEntry::Requested(record) = item.entry
                && record.namespace == namespace
                && record.request == request
            {
                if found.as_ref().is_some_and(|previous| previous != &record) {
                    return Err(RecordError::RequestConflict);
                }
                found = Some(record);
            }
        }
        Ok(found)
    }
    pub(super) fn lookup_request(
        &mut self,
        namespace: &str,
        request: &str,
    ) -> Result<Option<RequestRecord>, RecordError> {
        if self.poisoned {
            return Err(RecordError::Poisoned);
        }
        if let Some(record) = self.requests.get(namespace, request) {
            return Ok(Some(record));
        }
        if !self.requests.maybe(namespace, request) {
            return Ok(None);
        }
        let found = self.scan_request(namespace, request)?;
        if let Some(record) = &found {
            self.requests.remember(record.clone());
        }
        Ok(found)
    }
    pub(super) fn reserve_request(
        &mut self,
        record: RequestRecord,
        current: bool,
    ) -> Result<RequestClaim, RecordError> {
        record
            .validate()
            .map_err(|_| RecordError::Limit("request identity"))?;
        if let Some(previous) = self.lookup_request(&record.namespace, &record.request)? {
            if !previous.matches(&record) {
                return Err(RecordError::RequestConflict);
            }
            return Ok(RequestClaim {
                record: previous,
                fresh: false,
                persistence: match self.durability {
                    Durability::File => Persistence::FileSynced,
                    Durability::FileAndDirectory => Persistence::FileAndDirectorySynced,
                },
            });
        }
        if !current {
            return Err(RecordError::RequestContext);
        }
        let receipt = self.append(&Record::Journal(JournalEntry::Requested(record.clone())))?;
        Ok(RequestClaim {
            record,
            fresh: true,
            persistence: receipt.persistence,
        })
    }
    pub(super) fn remember_request(&mut self, record: &Record) {
        if let Record::Journal(JournalEntry::Requested(request)) = record {
            self.requests.remember(request.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn private_root() -> tempfile::TempDir {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        builder.tempdir().unwrap()
    }
    fn record(index: usize) -> RequestRecord {
        RequestRecord {
            namespace: "pane".into(),
            request: format!("r{index}"),
            cell: format!("c{index}"),
            fingerprint: format!("{index:064x}"),
            steps: Vec::new(),
        }
    }
    #[test]
    fn startup_warms_recent_requests_new_ids_do_not_scan_and_cold_ids_scan_once() {
        let root = private_root();
        let path = root.path().join("history");
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .unwrap();
        }
        #[cfg(not(unix))]
        std::fs::create_dir(&path).unwrap();
        drop(FileHistory::open(&path, ReadLimits::default(), Durability::File).unwrap());
        let mut bytes = Vec::new();
        for index in 0..HOT + 5 {
            bytes.extend(
                encode_journal(&JournalEntry::Requested(record(index)), Limits::default()).unwrap(),
            );
            bytes.push(b'\n');
        }
        let mut file = std::fs::OpenOptions::new();
        file.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            file.mode(0o600);
        }
        file.open(path.join("journal.jsonl"))
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        let mut history =
            FileHistory::open(&path, ReadLimits::default(), Durability::File).unwrap();
        assert_eq!(history.requests.hot.len(), HOT);
        assert_eq!(history.requests.order.len(), HOT);
        assert_eq!(history.requests.filter.len(), filter_bytes());
        assert_eq!(history.requests.scans.get(), 0);
        assert_eq!(
            history.find_request("pane", "r10004").unwrap(),
            Some(record(10004))
        );
        assert!(history.claim_request(record(20000), true).unwrap().fresh);
        assert_eq!(history.requests.scans.get(), 0);
        assert_eq!(history.find_request("pane", "r0").unwrap(), Some(record(0)));
        assert_eq!(history.requests.scans.get(), 1);
        assert_eq!(history.find_request("pane", "r0").unwrap(), Some(record(0)));
        assert_eq!(history.requests.scans.get(), 1);
        // False positives cost a bounded-memory scan; absence still permits exactly one claim.
        history.requests.filter.fill(255);
        assert!(history.find_request("absent", "absent").unwrap().is_none());
        assert!(history.claim_request(record(30000), true).unwrap().fresh);
        assert!(!history.claim_request(record(30000), true).unwrap().fresh);
        assert_eq!(history.requests.hot.len(), HOT);
        assert_eq!(history.requests.order.len(), HOT);
    }
    #[test]
    fn uncertain_sync_poisoning_never_forgets_the_written_reservation() {
        let root = private_root();
        let mut history =
            FileHistory::open(root.path(), ReadLimits::default(), Durability::File).unwrap();
        assert!(
            history
                .append_record(
                    &Record::Journal(JournalEntry::Requested(record(0))),
                    |_, _| Err(io::Error::other("synthetic sync failure"))
                )
                .is_err()
        );
        assert!(matches!(
            history.find_request("pane", "r0"),
            Err(RecordError::Poisoned)
        ));
        drop(history);
        let mut history =
            FileHistory::open(root.path(), ReadLimits::default(), Durability::File).unwrap();
        assert!(!history.claim_request(record(0), false).unwrap().fresh);
    }
}
