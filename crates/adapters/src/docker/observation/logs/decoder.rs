//! Incremental Docker framing shared by finite reads and subscriptions.
//! Frame sizes never determine allocations; only bounded per-channel line buffers do.
use super::{InvocationError, framing};

pub(super) struct Line {
    pub channel: usize,
    pub bytes: Vec<u8>,
    pub partial: bool,
    pub truncated: bool,
}
#[derive(Default)]
struct Pending {
    bytes: Vec<u8>,
    started: u64,
    discarding: bool,
}
pub(super) struct Decoder {
    tty: bool,
    limit: usize,
    header: [u8; 8],
    header_used: usize,
    remaining: usize,
    channel: usize,
    pending: [Pending; 3],
    sequence: u64,
}
impl Decoder {
    pub fn new(tty: bool, line_limit: usize) -> Self {
        Self {
            tty,
            limit: line_limit,
            header: [0; 8],
            header_used: 0,
            remaining: 0,
            channel: 0,
            pending: std::array::from_fn(|_| Pending::default()),
            sequence: 0,
        }
    }
    pub fn feed(
        &mut self,
        mut bytes: &[u8],
        emit: &mut impl FnMut(Line) -> Result<(), InvocationError>,
    ) -> Result<(), InvocationError> {
        if self.tty {
            return self.lines(2, bytes, emit);
        }
        while !bytes.is_empty() {
            if self.remaining == 0 {
                let count = (8 - self.header_used).min(bytes.len());
                self.header[self.header_used..self.header_used + count]
                    .copy_from_slice(&bytes[..count]);
                bytes = &bytes[count..];
                self.header_used += count;
                if self.header_used < 8 {
                    break;
                }
                self.channel = match self.header[0] {
                    1 => 0,
                    2 => 1,
                    _ => return Err(framing()),
                };
                if self.header[1..4] != [0, 0, 0] {
                    return Err(framing());
                }
                self.remaining =
                    u32::from_be_bytes(self.header[4..8].try_into().expect("header")) as usize;
                self.header_used = 0;
                if self.remaining == 0 {
                    continue;
                }
            }
            let count = self.remaining.min(bytes.len());
            self.lines(self.channel, &bytes[..count], emit)?;
            self.remaining -= count;
            bytes = &bytes[count..];
        }
        Ok(())
    }
    fn lines(
        &mut self,
        channel: usize,
        bytes: &[u8],
        emit: &mut impl FnMut(Line) -> Result<(), InvocationError>,
    ) -> Result<(), InvocationError> {
        for segment in bytes.split_inclusive(|b| *b == b'\n') {
            let complete = segment.ends_with(b"\n");
            let content = if complete {
                &segment[..segment.len() - 1]
            } else {
                segment
            };
            let pending = &mut self.pending[channel];
            if pending.discarding {
                if complete {
                    pending.discarding = false;
                }
                continue;
            }
            if pending.bytes.is_empty() {
                pending.started = self.sequence;
                self.sequence = self.sequence.saturating_add(1);
            }
            let room = self.limit - pending.bytes.len();
            pending
                .bytes
                .extend_from_slice(&content[..content.len().min(room)]);
            let truncated = content.len() > room;
            if truncated || complete {
                if complete && !truncated && pending.bytes.ends_with(b"\r") {
                    pending.bytes.pop();
                }
                emit(Line {
                    channel,
                    bytes: std::mem::take(&mut pending.bytes),
                    partial: truncated,
                    truncated,
                })?;
                pending.discarding = truncated && !complete;
            }
        }
        Ok(())
    }
    pub fn finish(
        mut self,
        local_cut: bool,
        emit: &mut impl FnMut(Line) -> Result<(), InvocationError>,
    ) -> Result<(), InvocationError> {
        // A cancelled reader does not call finish; a damaged EOF never flushes a misleading suffix.
        if !local_cut && (self.remaining != 0 || self.header_used != 0) {
            return Err(framing());
        }
        let mut channels = [0, 1, 2];
        channels.sort_by_key(|i| self.pending[*i].started);
        for channel in channels {
            let pending = &mut self.pending[channel];
            if !pending.bytes.is_empty() {
                emit(Line {
                    channel,
                    bytes: std::mem::take(&mut pending.bytes),
                    partial: true,
                    truncated: local_cut,
                })?;
            }
        }
        Ok(())
    }
}
