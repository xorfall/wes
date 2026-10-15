//! A declared raw leaf uses the native store, never a peer-selected path or handle.
use super::*;
use wes_engine::streams::{RecordingAppend, RecordingEnd, StreamRecording};

pub(super) struct Raw {
    writer: StreamRecording,
    definition: Recording,
    initial: Option<Value>,
    final_shell: Option<Value>,
    sequence: u64,
    pub until: tokio::time::Instant,
    pub end: RecordingEnd,
}
impl Raw {
    pub async fn reserve(
        definition: &Recording,
        sink: &StreamSink,
    ) -> Result<Self, InvocationError> {
        let writer = sink
            .begin_recording(
                definition.maximum,
                definition.contract.clone(),
                definition.fields.clone(),
            )
            .await?;
        Ok(Self {
            writer,
            definition: definition.clone(),
            initial: None,
            final_shell: None,
            sequence: 0,
            until: tokio::time::Instant::now() + definition.duration,
            end: RecordingEnd::Failed,
        })
    }
    pub fn header(
        &mut self,
        data: &[u8],
        opened: bool,
        token: &CancellationToken,
    ) -> Result<(), InvocationError> {
        if data.len() > super::super::record_bytes::HEADER_BYTES
            || (!opened && self.initial.is_some())
            || (opened && self.final_shell.is_some())
        {
            return Err(error("Session recording shell violates its bounded state"));
        }
        let value = protocol::value(data, &self.definition.contract, token)?;
        let mut leaf = value.data();
        for field in &self.definition.fields {
            let Data::Record(record) = leaf else {
                return Err(error(
                    "Session recording requires an empty Bytes placeholder",
                ));
            };
            leaf = record
                .get(field)
                .ok_or_else(|| error("Session recording requires an empty Bytes placeholder"))?;
        }
        if !matches!(leaf, Data::Bytes(bytes) if bytes.is_empty()) {
            return Err(error(
                "Session recording requires an empty Bytes placeholder",
            ));
        }
        if opened {
            self.final_shell = Some(value);
        } else {
            self.initial = Some(value);
        }
        Ok(())
    }
    pub fn ready(&self) -> Result<(), InvocationError> {
        self.initial
            .as_ref()
            .map(|_| ())
            .ok_or_else(|| error("Session readiness requires an admitted recording shell"))
    }
    pub fn append(
        &mut self,
        sequence: u64,
        data: &[u8],
        token: &CancellationToken,
    ) -> Result<RecordingAppend, InvocationError> {
        if self.initial.is_none()
            || self.final_shell.is_some()
            || self.sequence.checked_add(1) != Some(sequence)
            || data.len() > 87_386
        {
            return Err(error(
                "Session raw recording sequence, state or chunk budget is invalid",
            ));
        }
        let contract = wes_core::contracts::ContractRegistry::new()
            .resolve("Bytes")
            .expect("native bytes");
        let value = protocol::value(data, &contract, token)?;
        let Data::Bytes(bytes) = value.data() else {
            unreachable!("checked bytes")
        };
        if bytes.is_empty() || bytes.len() > wes_engine::storage::CHUNK_BYTES {
            return Err(error("Session raw recording chunk exceeds its byte budget"));
        }
        if self
            .writer
            .accepted_bytes()
            .saturating_add(bytes.len() as u64)
            > self.definition.maximum
        {
            self.end = RecordingEnd::Limit;
            return Err(error(
                "Session raw recording byte ceiling exceeded; no retry was performed",
            ));
        }
        self.sequence = sequence;
        Ok(self.writer.append(bytes.to_vec()))
    }
    pub fn confirm(&mut self) -> Result<(), InvocationError> {
        if self.final_shell.is_none() {
            return Err(error("Session End requires a final recording shell"));
        }
        self.end = RecordingEnd::Ended;
        Ok(())
    }
    pub async fn abort(self) -> Result<(), InvocationError> {
        self.writer.abort().await
    }
    pub async fn finish(self) -> Result<(), InvocationError> {
        let shell = if self.end == RecordingEnd::Ended {
            self.final_shell
        } else {
            self.initial
        };
        match shell {
            Some(shell) => self.writer.finish(shell, self.end).await,
            None => self.writer.abort().await,
        }
    }
}
