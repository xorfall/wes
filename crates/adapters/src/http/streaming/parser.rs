//! Bounded SSE data framing. Unknown fields never create state or implicit reconnection policy.
use super::Failure;

pub(super) struct Parser {
    line: Vec<u8>,
    event: Vec<u8>,
    data: bool,
    first: bool,
    after_cr: bool,
    line_limit: usize,
    event_limit: usize,
}
impl Parser {
    pub fn new(line_limit: usize, event_limit: usize) -> Self {
        Self {
            line: vec![],
            event: vec![],
            data: false,
            first: true,
            after_cr: false,
            line_limit,
            event_limit,
        }
    }
    pub fn feed(
        &mut self,
        input: &[u8],
        mut deliver: impl FnMut(&[u8]) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        for &byte in input {
            if self.after_cr {
                self.after_cr = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte == b'\n' || byte == b'\r' {
                self.after_cr = byte == b'\r';
                self.end_line(&mut deliver)?;
            } else {
                if self.line.len() >= self.line_limit {
                    return Err(Failure::Size);
                }
                self.line.push(byte);
            }
        }
        Ok(())
    }
    fn end_line(
        &mut self,
        deliver: &mut impl FnMut(&[u8]) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let mut line = self.line.as_slice();
        if self.first {
            self.first = false;
            line = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line);
        }
        if line.is_empty() {
            if self.data {
                deliver(&self.event)?;
            }
            self.event.clear();
            self.data = false;
        } else {
            let (field, value) = match line.iter().position(|&b| b == b':') {
                Some(index) => (
                    &line[..index],
                    line[index + 1..]
                        .strip_prefix(b" ")
                        .unwrap_or(&line[index + 1..]),
                ),
                None => (line, &b""[..]),
            };
            if field == b"data" {
                let extra = value
                    .len()
                    .checked_add(usize::from(self.data))
                    .ok_or(Failure::Size)?;
                if extra > self.event_limit.saturating_sub(self.event.len()) {
                    return Err(Failure::Size);
                }
                if self.data {
                    self.event.push(b'\n');
                }
                self.event.extend_from_slice(value);
                self.data = true;
            }
        }
        self.line.clear();
        Ok(())
    }
    // EOF intentionally drops an incomplete event; only a blank line dispatches data.
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(input: &[u8], chunk: usize) -> Vec<Vec<u8>> {
        let mut parser = Parser::new(1024, 2048);
        let mut events = vec![];
        for part in input.chunks(chunk) {
            assert!(
                parser
                    .feed(part, |event| {
                        events.push(event.to_vec());
                        Ok(())
                    })
                    .is_ok()
            );
        }
        events
    }
    #[test]
    fn bom_utf8_and_all_line_endings_are_independent_of_chunk_boundaries() {
        let input =
            "\u{feff}:keepalive\r\ndata: \"Türkçe 🚀\"\r\rdata: {\ndata: \"n\":1}\n\n".as_bytes();
        for size in 1..=input.len() {
            assert_eq!(
                parse(input, size),
                ["\"Türkçe 🚀\"".as_bytes(), b"{\n\"n\":1}"].map(<[u8]>::to_vec)
            );
        }
    }
    #[test]
    fn empty_data_lines_and_fields_without_colons_preserve_newlines() {
        assert_eq!(
            parse(
                b"event: custom\nid: x\nretry: 0\ndata\ndata:\ndata:  x\n\ndata:\n\n",
                1
            ),
            [b"\n\n x".to_vec(), vec![]]
        );
    }
    #[test]
    fn eof_never_dispatches_incomplete_events_or_unknown_fields() {
        for input in [
            &b"data: 1"[..],
            b"data: 1\n",
            b"data: 1\r",
            b":comment\n\n",
            b"unknown: x\n\n",
        ] {
            assert!(parse(input, 1).is_empty());
        }
        assert_eq!(parse(b"data: 1\n\ndata: 2\n", 2), [b"1".to_vec()]);
    }
    #[test]
    fn line_and_aggregate_event_limits_check_before_append_including_empty_lines() {
        let mut line = Parser::new(4, 8);
        assert!(line.feed(b"1234", |_| Ok(())).is_ok());
        assert!(matches!(line.feed(b"5", |_| Ok(())), Err(Failure::Size)));
        assert_eq!(line.line.len(), 4);
        let mut event = Parser::new(64, 4);
        assert!(event.feed(b"data: ab\ndata: c\n", |_| Ok(())).is_ok());
        assert_eq!(event.event, b"ab\nc");
        assert!(matches!(
            event.feed(b"data:\n", |_| Ok(())),
            Err(Failure::Size)
        ));
        assert_eq!(event.event, b"ab\nc");
    }
    #[test]
    fn bom_is_stripped_only_once_and_callback_failure_stops_consumption() {
        assert_eq!(
            parse(b"\xef\xbb\xbfdata: 1\n\n\xef\xbb\xbfdata: 2\n\n", 1),
            [b"1".to_vec()]
        );
        let mut parser = Parser::new(64, 64);
        let mut count = 0;
        assert!(matches!(
            parser.feed(b"data: 1\n\ndata: 2\n\n", |_| {
                count += 1;
                Err(Failure::Response)
            }),
            Err(Failure::Response)
        ));
        assert_eq!(count, 1);
    }
}
