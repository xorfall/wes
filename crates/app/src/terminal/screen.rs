//! Bounded presentation owned by the PTY, independent of a connected renderer.
//!
//! The VT model owns drawing state and normal history; this adapter retains the
//! UTF-8 tail and xterm's input modes. A recovery contains only drawing/mode
//! instructions, never the original terminal queries, bells or clipboard requests.
use avt::parser::{Function, Parser, State};

pub(super) const HISTORY_ROWS: usize = 5000;
const PARSE_BATCH: usize = 64;

pub(super) struct Screen {
    vt: avt::Vt,
    observer: Parser,
    modes: InputModes,
    cell_limit: usize,
    utf8: Vec<u8>,
}
impl Screen {
    pub fn new(cell_limit: usize) -> Self {
        Self {
            vt: avt::Vt::builder()
                .scrollback_limit(HISTORY_ROWS.min(cell_limit / 80))
                .build(),
            observer: Parser::new(),
            modes: InputModes::default(),
            cell_limit,
            utf8: Vec::with_capacity(4),
        }
    }
    pub fn size(&self) -> (u16, u16) {
        let (cols, rows) = self.vt.size();
        (cols as u16, rows as u16)
    }
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.vt
            .set_scrollback_limit(HISTORY_ROWS.min(self.cell_limit / usize::from(cols)));
        drop(self.vt.resize(cols.into(), rows.into()));
        self.vt
            .set_scrollback_limit(HISTORY_ROWS.min(self.cell_limit / usize::from(cols)));
    }
    pub fn append(&mut self, bytes: &[u8]) {
        // The endpoint's read buffer is finite. The decoder carries at most three
        // incomplete bytes, including across output journal eviction.
        for bytes in bytes.chunks(16 * 1024) {
            let mut input = std::mem::take(&mut self.utf8);
            input.extend_from_slice(bytes);
            let mut remaining = input.as_slice();
            loop {
                match std::str::from_utf8(remaining) {
                    Ok(text) => {
                        self.text(text);
                        break;
                    }
                    Err(error) => {
                        let valid = error.valid_up_to();
                        self.text(
                            std::str::from_utf8(&remaining[..valid]).expect("valid UTF-8 prefix"),
                        );
                        remaining = &remaining[valid..];
                        if let Some(len) = error.error_len() {
                            self.text("\u{fffd}");
                            remaining = &remaining[len..];
                        } else {
                            self.utf8.extend_from_slice(remaining);
                            break;
                        }
                    }
                }
            }
        }
    }
    fn text(&mut self, text: &str) {
        for (index, ch) in text.chars().enumerate() {
            let state = self.observer.state;
            self.modes.feed(state, ch);
            let operation = self.observer.feed(ch);
            // Geometry belongs to a successfully resized endpoint, not to an
            // untrusted window-operation escape emitted by its child process.
            if matches!(
                operation,
                Some(Function::Xtwinops(avt::parser::XtwinopsOp::Resize(..)))
            ) {
                self.vt.feed('\u{18}');
            } else {
                self.vt.feed(ch);
            }
            // Do not let one producer burst accumulate unbounded retired rows
            // inside the emulator before its collection boundary.
            if index % PARSE_BATCH == PARSE_BATCH - 1 {
                self.collect();
            }
        }
        self.collect();
    }
    fn collect(&mut self) {
        drop(self.vt.feed_str(""));
    }
    pub fn complete(&self) -> bool {
        self.vt.snapshot_complete()
    }
    pub fn snapshot(&self) -> Vec<u8> {
        let mut bytes = String::new();
        // A snapshot is a complete drawing, not an unfinished synchronized
        // update. The next raw begin/end sequences resume normal synchronization.
        bytes.push_str("\x1b[?2026l");
        if self.vt.primary_root_wrapped() {
            // Preserve the first retained row's continuation marker even when
            // its predecessor has expired. Delete the synthetic predecessor
            // before drawing any real row; it never enters retained history.
            bytes.push_str(&" ".repeat(self.vt.size().0));
            bytes.push_str(" \x1b[H\x1b[M\x1b[H");
        }
        let history = self
            .vt
            .primary_lines()
            .count()
            .saturating_sub(self.vt.size().1);
        if history != 0 {
            let mut wrapped = false;
            for (index, row) in self.vt.primary_lines().take(history).enumerate() {
                if index != 0 && !wrapped {
                    bytes.push_str("\r\n");
                }
                let mut pen = avt::Pen::default();
                bytes.push_str("\x1b[0m");
                for cell in row.cells() {
                    if cell.width() == 0 {
                        continue;
                    }
                    if *cell.pen() != pen {
                        pen = *cell.pen();
                        write_pen(&mut bytes, &pen);
                    }
                    bytes.extend(cell.chars());
                }
                wrapped = row.is_wrapped();
            }
            // Make room for the current screen without pushing its cursor-
            // positioned redraw into the retained history. Trigger a final soft
            // wrap explicitly, preserving its row marker in xterm.
            if wrapped {
                bytes.push_str(" ");
            } else {
                bytes.push_str("\r\n");
            }
            for _ in 1..self.vt.size().1 {
                bytes.push_str("\r\n");
            }
            bytes.push_str("\x1b[H\x1b[0m");
        }
        self.modes.write(&mut bytes);
        // avt's state dump restores both screen contexts, margins, saved cursor,
        // attributes, tabs, character sets and its unfinished VT parser state.
        bytes.push_str(&self.vt.dump());
        let mut bytes = bytes.into_bytes();
        bytes.extend_from_slice(&self.utf8);
        bytes
    }
}

fn write_pen(output: &mut String, pen: &avt::Pen) {
    use std::fmt::Write;
    output.push_str("\x1b[0");
    for (enabled, code) in [
        (pen.is_bold(), 1),
        (pen.is_faint(), 2),
        (pen.is_italic(), 3),
        (pen.is_underline(), 4),
        (pen.is_blink(), 5),
        (pen.is_inverse(), 7),
        (pen.is_strikethrough(), 9),
    ] {
        if enabled {
            write!(output, ";{code}").expect("string write");
        }
    }
    for (color, code) in [(pen.foreground(), 38), (pen.background(), 48)] {
        match color {
            Some(avt::Color::Indexed(index)) if index < 8 => {
                write!(output, ";{}", code - 8 + index).expect("string write")
            }
            Some(avt::Color::Indexed(index)) if index < 16 => {
                write!(output, ";{}", code + 44 + index).expect("string write")
            }
            Some(avt::Color::Indexed(index)) => {
                write!(output, ";{code};5;{index}").expect("string write")
            }
            Some(avt::Color::RGB(rgb)) => {
                write!(output, ";{code};2;{};{};{}", rgb.r, rgb.g, rgb.b).expect("string write")
            }
            None => {}
        }
    }
    output.push('m');
}

/// Only modes outside the drawing emulator: a fixed set, not an unbounded list
/// of escape strings. Control strings and numeric parameters stay bounded.
#[derive(Default)]
struct InputModes {
    keypad: bool,
    mouse: u16,
    encoding: u16,
    focus: bool,
    paste: bool,
    private: bool,
    params: [u16; 32],
    parameter: usize,
}
impl InputModes {
    fn feed(&mut self, state: State, ch: char) {
        if state == State::Escape {
            match ch {
                '=' => self.keypad = true,
                '>' => self.keypad = false,
                'c' => *self = Self::default(),
                '[' => {
                    self.private = false;
                    self.params.fill(0);
                    self.parameter = 0;
                }
                _ => {}
            }
        }
        if ch == '\u{9b}' {
            self.private = false;
            self.params.fill(0);
            self.parameter = 0;
        }
        if !matches!(state, State::CsiEntry | State::CsiParam) {
            return;
        }
        match ch {
            '?' if state == State::CsiEntry => self.private = true,
            '0'..='9' => {
                self.params[self.parameter] = self.params[self.parameter]
                    .saturating_mul(10)
                    .saturating_add(ch as u16 - '0' as u16)
            }
            ';' => self.parameter = (self.parameter + 1).min(self.params.len() - 1),
            'h' | 'l' if self.private => {
                let enabled = ch == 'h';
                for mode in &self.params[..=self.parameter] {
                    match *mode {
                        9 | 1000 | 1002 | 1003 => {
                            if enabled {
                                self.mouse = *mode;
                            } else if self.mouse == *mode {
                                self.mouse = 0;
                            }
                        }
                        1005 | 1006 | 1016 => self.encoding = if enabled { *mode } else { 0 },
                        1004 => self.focus = enabled,
                        2004 => self.paste = enabled,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    fn write(&self, output: &mut String) {
        use std::fmt::Write;
        if self.keypad {
            output.push_str("\x1b=");
        }
        for mode in [
            self.mouse,
            self.encoding,
            if self.focus { 1004 } else { 0 },
            if self.paste { 2004 } else { 0 },
        ] {
            if mode != 0 {
                write!(output, "\x1b[?{mode}h").expect("string write");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_is_charged_by_physical_cells_and_reset_clears_it() {
        let mut screen = Screen::new(80 * 10);
        screen.append(b"line\r\n".repeat(2000).as_slice());
        assert!(screen.vt.primary_lines().count() <= 24 + 10);
        assert!(screen.utf8.len() <= 3);
        screen.append(b"\x1bc");
        assert_eq!(screen.vt.primary_lines().count(), 24);
    }
    #[test]
    fn recovery_carries_partial_utf8_and_vt_state_without_queries() {
        let mut screen = Screen::new(1000);
        screen.append(b"\x1b[?1000;1006;2004h\x1b[6n\x1b]52;c;SECRET\x07\x1b[?202");
        let snapshot = screen.snapshot();
        let text = String::from_utf8(snapshot).unwrap();
        assert!(
            text.contains("\x1b[?1000h")
                && text.contains("\x1b[?1006h")
                && text.contains("\x1b[?2004h")
        );
        assert!(!text.contains("SECRET") && !text.contains("[6n"));
        assert!(text.ends_with("\u{9b}?202"));
        screen.append(b"6h\xc4");
        assert_eq!(screen.snapshot().last(), Some(&0xc4));
        screen.append(b"\x9e");
        assert!(screen.vt.text().iter().any(|line| line.contains('Ğ')));
    }
    #[test]
    fn a_pathological_cell_is_refused_and_an_explicit_reset_recovers_capacity() {
        let mut screen = Screen::new(400000);
        screen.append(format!("a{}", "\u{301}".repeat(10000)).as_bytes());
        assert!(!screen.complete());
        assert_eq!(screen.size(), (80, 24));
        screen.append(b"\x1b[1;1Hx");
        assert!(screen.complete()); // The only incomplete cell was replaced.
        screen.append(format!("b{}", "\u{301}".repeat(10000)).as_bytes());
        assert!(!screen.complete());
        screen.append(b"\x1bc");
        assert!(screen.complete());
    }
    #[test]
    fn native_snapshots_match_the_shipped_xterm_parser() {
        use base64::Engine;
        use serde_json::json;
        use std::io::Write;
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let history = (0..100)
            .map(|index| format!("\x1b[32mrow {index:03}\x1b[0m\r\n"))
            .collect::<String>();
        let cases: Vec<(&str, Vec<u8>, Vec<u8>)> = vec![
            (
                "normal history and incremental progress",
                format!("{history}\x1b[20;1H\x1b[2Kworking\x1b[?2026h").into_bytes(),
                b"\x1b[?2026l\x1b[20;1Hdone".to_vec(),
            ),
            (
                "input modes and pending private CSI",
                b"\x1b=\x1b[?1;1000;1006;2004h\x1b[?202".to_vec(),
                b"6hhello\x1b[?2026l".to_vec(),
            ),
            (
                "split UTF-8",
                b"Turkish \xc4".to_vec(),
                "ĞğİıŞşÇçÖöÜü".as_bytes()[1..].to_vec(),
            ),
            (
                "alternate screen return",
                format!("{history}\x1b[?1049h\x1b[5;9HALT\x1b7\x1b[8;3Hselected").into_bytes(),
                b"\x1b8!\x1b[?1049lcontinued".to_vec(),
            ),
            (
                "margins attributes saved cursor and charset",
                b"\x1b[2;22r\x1b[?6h\x1b[3;5H\x1b[31;1mred\x1b7\x1b[9;5H\x1b(0qqq".to_vec(),
                b"\x1b8more\r\nlast".to_vec(),
            ),
            (
                "no replay of terminal queries",
                b"hello\x1b[6n\x1b]52;c;c2VudGluZWw=\x07".to_vec(),
                b" world".to_vec(),
            ),
            (
                "wrapped Unicode rows",
                "東京界\r\n".repeat(200).into_bytes(),
                b"next".to_vec(),
            ),
            (
                "combining marks",
                "Cafe\u{301} G\u{306} I\u{307} 東京\u{301}"
                    .as_bytes()
                    .to_vec(),
                b" next".to_vec(),
            ),
            (
                "output cannot allocate new terminal geometry",
                b"\x1b[8;65535;65535thello".to_vec(),
                b" next".to_vec(),
            ),
        ];
        let mut fixtures = vec![];
        for (name, prefix, suffix) in cases {
            let mut screen = Screen::new(400000);
            // Exercise the native persistent decoder at every possible byte boundary.
            for byte in &prefix {
                screen.append(&[*byte]);
            }
            let (cols, rows) = screen.size();
            fixtures.push(json!({"name":name,"cols":cols,"rows":rows,"events":[{"data":encode(&prefix)}],"snapshot":encode(&screen.snapshot()),"suffix":encode(&suffix)}));
        }
        for (name, sizes, alternate) in [
            (
                "growing screen pulls retained history",
                vec![(80, 30)],
                false,
            ),
            (
                "narrow and grow reflows retained history",
                vec![(40, 24), (40, 30)],
                false,
            ),
            (
                "alternate resize preserves primary history",
                vec![(100, 30)],
                true,
            ),
        ] {
            let mut screen = Screen::new(400000);
            screen.append(history.as_bytes());
            let mut events = vec![json!({"data":encode(history.as_bytes())})];
            if alternate {
                screen.append(b"\x1b[?1049hALT");
                events.push(json!({"data":encode(b"\x1b[?1049hALT")}));
            }
            for (cols, rows) in sizes {
                screen.resize(cols, rows);
                events.push(json!({"resize":[cols,rows]}));
            }
            let (cols, rows) = screen.size();
            let suffix: &[u8] = if alternate {
                b"\x1b[?1049lcontinued"
            } else {
                b"continued"
            };
            fixtures.push(json!({"name":name,"cols":cols,"rows":rows,"events":events,"snapshot":encode(&screen.snapshot()),"suffix":encode(suffix)}));
        }
        {
            let mut prefix = b"\x1b[?2026h".to_vec();
            prefix.extend(vec![b'x'; 1024 * 1024]);
            let mut screen = Screen::new(400000);
            screen.append(&prefix);
            fixtures.push(json!({"name":"mode prefix beyond raw retention","cols":80,"rows":24,"events":[{"data":encode(&prefix)}],"snapshot":encode(&screen.snapshot()),"suffix":encode(b"\x1b[?2026l") }));
        }
        let mut child = std::process::Command::new("node")
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../examples/terminal/recovery-xterm.cjs"),
            )
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("Node with installed GUI dependencies");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(&fixtures).unwrap())
            .unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
}
