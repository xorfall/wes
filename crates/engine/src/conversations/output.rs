use super::{Channel, ConversationError, Output};

#[derive(Default)]
struct Decoder {
    pending: Vec<u8>,
}
impl Decoder {
    fn write(&mut self, mut bytes: &[u8], mut emit: impl FnMut(&str)) {
        // Complete a split scalar without copying the incoming chunk. Each lane holds at most
        // three incomplete bytes; stdout and stderr never complete each other's UTF-8 sequences.
        while !self.pending.is_empty() && !bytes.is_empty() {
            self.pending.push(bytes[0]);
            bytes = &bytes[1..];
            let saved = std::mem::take(&mut self.pending);
            self.decode(&saved, &mut emit);
        }
        if !bytes.is_empty() {
            self.decode(bytes, &mut emit);
        }
    }
    fn decode(&mut self, mut bytes: &[u8], emit: &mut impl FnMut(&str)) {
        while !bytes.is_empty() {
            match std::str::from_utf8(bytes) {
                Ok(text) => {
                    emit(text);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    emit(std::str::from_utf8(&bytes[..valid]).expect("validated prefix"));
                    bytes = &bytes[valid..];
                    match error.error_len() {
                        Some(invalid) => {
                            emit("\u{fffd}");
                            bytes = &bytes[invalid..];
                        }
                        None => {
                            self.pending.extend_from_slice(bytes);
                            break;
                        }
                    }
                }
            }
        }
    }
}

pub(super) struct Buffer {
    limit: usize,
    text: String,
    omitted: u64,
    decoders: [Decoder; 2],
}
impl Buffer {
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            text: String::new(),
            omitted: 0,
            decoders: Default::default(),
        }
    }
    fn append(&mut self, text: &str) -> Result<(), ConversationError> {
        // Once output is lost keep one contiguous prefix, rather than interspersing later small
        // fragments with missing text. The next drain opens a fresh bounded batch.
        let mut end = if self.omitted == 0 {
            text.len().min(self.limit - self.text.len())
        } else {
            0
        };
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let lost = u64::try_from(text.len() - end).map_err(|_| ConversationError::Capacity)?;
        self.omitted = self
            .omitted
            .checked_add(lost)
            .ok_or(ConversationError::Capacity)?;
        self.text.push_str(&text[..end]);
        Ok(())
    }
    pub fn write(&mut self, channel: Channel, bytes: &[u8]) -> Result<(), ConversationError> {
        let index = match channel {
            Channel::Stdout => 0,
            Channel::Stderr => 1,
        };
        let mut decoder = std::mem::take(&mut self.decoders[index]);
        let mut result = Ok(());
        decoder.write(bytes, |text| {
            if result.is_ok() {
                result = self.append(text);
            }
        });
        self.decoders[index] = decoder;
        result
    }
    pub fn finish(&mut self) -> Result<(), ConversationError> {
        for index in 0..2 {
            if !self.decoders[index].pending.is_empty() {
                self.decoders[index].pending.clear();
                self.append("\u{fffd}")?;
            }
        }
        Ok(())
    }
    pub fn take(&mut self) -> Output {
        Output {
            text: std::mem::take(&mut self.text),
            omitted_bytes: std::mem::take(&mut self.omitted),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_split_matches_lossy_utf8_and_independent_lanes_do_not_cross_complete() {
        let bytes = b"hello \xf0\x9f\x8e\x89\xc3\xa7\xff\xe2\x82x\xf0\x9f";
        for chunk in 1..=bytes.len() {
            let mut buffer = Buffer::new(1024);
            for bytes in bytes.chunks(chunk) {
                buffer.write(Channel::Stdout, bytes).unwrap();
            }
            buffer.finish().unwrap();
            assert_eq!(buffer.take().text, String::from_utf8_lossy(bytes));
        }
        let mut buffer = Buffer::new(1024);
        buffer.write(Channel::Stdout, b"\xc3").unwrap();
        buffer.write(Channel::Stderr, b"x").unwrap();
        buffer.write(Channel::Stdout, b"\xa7").unwrap();
        assert_eq!(buffer.take().text, "xç");
    }
    #[test]
    fn oversized_chunks_are_strictly_bounded_with_explicit_loss_and_fresh_drains() {
        let mut buffer = Buffer::new(5);
        buffer
            .write(Channel::Stdout, "ab🎉later".as_bytes())
            .unwrap();
        buffer.write(Channel::Stdout, b"x").unwrap();
        let output = buffer.take();
        assert_eq!(output.text, "ab");
        assert_eq!(output.omitted_bytes, 10);
        buffer
            .write(Channel::Stderr, &vec![b'x'; 1024 * 1024])
            .unwrap();
        let output = buffer.take();
        assert_eq!(output.text, "xxxxx");
        assert_eq!(output.omitted_bytes, 1024 * 1024 - 5);
        buffer.write(Channel::Stdout, b"ok").unwrap();
        assert_eq!(buffer.take().text, "ok");
    }
}
