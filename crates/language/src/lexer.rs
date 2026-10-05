use std::sync::LazyLock;

use regex::Regex;

use crate::{Diagnostic, SourceText, Span};

static NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\p{L}\p{Nd}_-]$").expect("the constant name character expression is valid")
});

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Word,
    Calc,
    Def,
    Signature,
    ReturnType,
    Key,
    Ref,
    String,
    Structured,
    Bind,
    ErrorBind,
    Pipe,
    Meta,
    At,
    ParenOpen,
    ParenClose,
    BraceOpen,
    BraceClose,
    Comma,
    Newline,
    Eof,
}

impl TokenKind {
    /// Acceptance fixture name; this is not an end-user wire representation.
    pub fn fixture_name(self) -> &'static str {
        match self {
            Self::Word => "WORD",
            Self::Calc => "CALC",
            Self::Def => "DEF",
            Self::Signature => "SIGNATURE",
            Self::ReturnType => "RETURN_TYPE",
            Self::Key => "KEY",
            Self::Ref => "REF",
            Self::String => "STRING",
            Self::Structured => "STRUCTURED",
            Self::Bind => "BIND",
            Self::ErrorBind => "ERROR_BIND",
            Self::Pipe => "PIPE",
            Self::Meta => "META",
            Self::At => "AT",
            Self::ParenOpen => "PAREN_OPEN",
            Self::ParenClose => "PAREN_CLOSE",
            Self::BraceOpen => "BRACE_OPEN",
            Self::BraceClose => "BRACE_CLOSE",
            Self::Comma => "COMMA",
            Self::Newline => "NEWLINE",
            Self::Eof => "EOF",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub text: String,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn lex(source: &SourceText) -> Lexed {
    Scanner {
        text: source.text(),
        pos: 0,
        header: false,
        header_words: 0,
        annotation: false,
        output: Lexed {
            tokens: Vec::new(),
            diagnostics: Vec::new(),
        },
    }
    .scan()
}

struct Scanner<'a> {
    text: &'a str,
    pos: usize,
    header: bool,
    header_words: usize,
    annotation: bool,
    output: Lexed,
}

/// Language whitespace excludes non-breaking space characters.
pub(crate) fn whitespace(ch: char) -> bool {
    matches!(ch, '\u{9}'..='\u{d}' | '\u{1c}'..='\u{20}' | '\u{1680}' |
        '\u{2000}'..='\u{2006}' | '\u{2008}'..='\u{200a}' | '\u{2028}' | '\u{2029}' |
        '\u{205f}' | '\u{3000}')
}

/// Whether text can be written without quotes after a named-argument colon.
pub fn bare_named_text(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with(['$', '"', '[', '{'])
        && !text
            .chars()
            .any(|ch| whitespace(ch) || matches!(ch, '>' | '|' | '{' | '}' | '"' | '\\'))
}

pub fn binding_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(binding_character)
}

pub(crate) fn binding_character(ch: char) -> bool {
    static BINDING: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[\p{L}\p{Nd}_]$").expect("binding character grammar"));
    let mut buffer = [0; 4];
    BINDING.is_match(ch.encode_utf8(&mut buffer))
}

fn name_character(ch: char) -> bool {
    // Names admit single UTF-16 code units; supplementary letters are excluded.
    if ch.len_utf16() != 1 {
        return false;
    }
    let mut buffer = [0; 4];
    NAME.is_match(ch.encode_utf8(&mut buffer))
}

impl Scanner<'_> {
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }
    fn advance(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }
    fn span(&self, start: usize) -> Span {
        Span::new(start, self.pos).expect("scanner position is monotonic")
    }
    fn emit(&mut self, kind: TokenKind, text: impl Into<String>, start: usize) {
        self.output.tokens.push(Token {
            kind,
            text: text.into(),
            span: self.span(start),
        });
    }
    fn error(&mut self, code: &'static str, start: usize, message: impl Into<String>) {
        self.output
            .diagnostics
            .push(Diagnostic::error(code, self.span(start), message));
    }
    fn scan(mut self) -> Lexed {
        while let Some(ch) = self.peek() {
            if ch == '\n' {
                self.header = false;
                let start = self.pos;
                self.advance();
                self.emit(TokenKind::Newline, "", start);
            } else if self.text[self.pos..].starts_with("//") {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.advance();
                }
            } else if whitespace(ch) {
                self.advance();
            } else {
                self.chunk(ch);
            }
        }
        self.emit(TokenKind::Eof, "", self.pos);
        self.output
    }
    fn chunk(&mut self, ch: char) {
        let start = self.pos;
        let tail = &self.text[start..];
        if tail.starts_with(":calc")
            && tail[5..]
                .chars()
                .next()
                .is_none_or(|c| whitespace(c) || c == '{')
        {
            match crate::calc::context_end(self.text, start) {
                Ok(end) => {
                    self.pos = end;
                    self.emit(TokenKind::Calc, self.text[start..end].to_owned(), start);
                }
                Err(error) => {
                    self.output.diagnostics.push(error);
                    self.pos = self.text.len();
                }
            }
            return;
        }
        if tail.starts_with(":def")
            && tail[4..].chars().next().is_none_or(whitespace)
            && self
                .output
                .tokens
                .last()
                .is_none_or(|token| token.kind == TokenKind::Newline)
        {
            self.pos += 4;
            self.header = true;
            self.header_words = 0;
            self.emit(TokenKind::Def, ":def", start);
            return;
        }
        if self.header && tail.starts_with("->") {
            self.pos += 2;
            while self.peek().is_some_and(whitespace) {
                self.advance();
            }
            let content = self.pos;
            let mut depth = 0i32;
            while let Some(c) = self.peek() {
                if depth == 0 && whitespace(c) {
                    break;
                }
                if c == '\n' {
                    break;
                }
                if c == '<' {
                    depth += 1;
                }
                if c == '>' {
                    depth -= 1;
                }
                self.advance();
                if depth < 0 {
                    break;
                }
            }
            self.emit(
                TokenKind::ReturnType,
                self.text[content..self.pos].to_owned(),
                start,
            );
            return;
        }
        if self.header && ch == '(' {
            self.advance();
            let content = self.pos;
            while self.peek().is_some_and(|c| c != ')' && c != '\n') {
                self.advance();
            }
            let signature = self.text[content..self.pos].to_string();
            if self.peek() == Some(')') {
                self.advance();
            } else {
                self.error(
                    "LEX005",
                    start,
                    "template parameter signature was never closed",
                );
            }
            self.emit(TokenKind::Signature, signature, start);
            return;
        }
        let kind = match ch {
            '*' if tail.starts_with("*>") => {
                self.pos += 2;
                self.emit(TokenKind::ErrorBind, "*>", start);
                return;
            }
            '@' => TokenKind::At,
            '(' if self.output.tokens.len() >= 2
                && self.output.tokens[self.output.tokens.len() - 2].kind == TokenKind::At =>
            {
                self.annotation = true;
                TokenKind::ParenOpen
            }
            ')' if self.annotation => {
                self.annotation = false;
                TokenKind::ParenClose
            }
            '{' => {
                self.annotation = self.output.tokens.len() >= 2
                    && self.output.tokens[self.output.tokens.len() - 2].kind == TokenKind::At;
                TokenKind::BraceOpen
            }
            '}' => {
                self.annotation = false;
                TokenKind::BraceClose
            }
            ',' => TokenKind::Comma,
            ':' => TokenKind::Meta,
            '>' => TokenKind::Bind,
            '|' => TokenKind::Pipe,
            '"' => {
                self.string();
                return;
            }
            '$' => {
                self.reference();
                return;
            }
            _ => {
                self.word();
                return;
            }
        };
        self.advance();
        self.emit(kind, ch.to_string(), start);
    }
    fn ends_run(&self, ch: char) -> bool {
        whitespace(ch)
            || (self.header && ch == '(')
            || matches!(ch, '>' | '|' | '{' | '}')
            || (self.annotation && matches!(ch, ',' | ')'))
            || (ch == '('
                && self
                    .output
                    .tokens
                    .last()
                    .is_some_and(|t| t.kind == TokenKind::At))
    }
    fn word(&mut self) {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|ch| !self.ends_run(ch) && ch != ':')
        {
            self.advance();
        }
        let word = self.text[start..self.pos].to_string();
        if self.peek() != Some(':') {
            self.emit(TokenKind::Word, &word, start);
            if self.header {
                self.header_words += 1;
                if self.header_words == 2 && word == "as" {
                    self.header = false;
                }
            }
        } else {
            self.advance();
            self.emit(TokenKind::Key, &word, start);
            self.value(&word);
        }
    }
    fn value(&mut self, key: &str) {
        match self.peek() {
            Some('{' | '[') => self.structure(),
            None => self.error(
                "LEX003",
                self.pos,
                format!("expected a value after '{key}:'"),
            ),
            Some(ch) if self.ends_run(ch) => self.error(
                "LEX003",
                self.pos,
                format!("expected a value after '{key}:'"),
            ),
            Some('"') => self.string(),
            Some('$') => self.reference(),
            _ => {
                let start = self.pos;
                while self.peek().is_some_and(|ch| !self.ends_run(ch)) {
                    self.advance();
                }
                if !bare_named_text(&self.text[start..self.pos]) {
                    self.error(
                        "LEX006",
                        start,
                        "Quote named text containing a quote or backslash.",
                    );
                }
                self.emit(
                    TokenKind::Word,
                    self.text[start..self.pos].to_string(),
                    start,
                );
            }
        }
    }
    fn structure(&mut self) {
        let start = self.pos;
        let mut stack = Vec::new();
        let mut quoted = false;
        let mut escaped = false;
        while let Some(ch) = self.peek() {
            if self.pos - start > crate::structured::MAX_BYTES {
                self.error(
                    "ARG001",
                    start,
                    "Structured argument exceeds its byte budget.",
                );
                break;
            }
            let comment_boundary = self.pos == start
                || self.text[start..self.pos]
                    .chars()
                    .next_back()
                    .is_some_and(|ch| {
                        whitespace(ch) || matches!(ch, '{' | '[' | ':' | ',' | '"' | ']' | '}')
                    });
            if !quoted && comment_boundary && self.text[self.pos..].starts_with("//") {
                while self.peek().is_some_and(|ch| ch != '\n') {
                    self.advance();
                }
                continue;
            }
            self.advance();
            if quoted {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    quoted = false;
                }
                continue;
            }
            match ch {
                '"' => quoted = true,
                '{' | '[' => {
                    if stack.len() >= crate::structured::MAX_DEPTH {
                        self.error(
                            "ARG001",
                            start,
                            "Structured argument exceeds its nesting budget.",
                        );
                        break;
                    }
                    stack.push(if ch == '{' { '}' } else { ']' });
                }
                '}' | ']' => {
                    if stack.pop() != Some(ch) {
                        self.error("ARG001", start, "Mismatched structured argument delimiter.");
                        break;
                    }
                    if stack.is_empty() {
                        self.emit(
                            TokenKind::Structured,
                            self.text[start..self.pos].to_owned(),
                            start,
                        );
                        return;
                    }
                }
                _ => (),
            }
        }
        self.error(
            "ARG001",
            start,
            "Structured argument must close its record or list.",
        );
        self.emit(
            TokenKind::Structured,
            self.text[start..self.pos].to_owned(),
            start,
        );
    }
    fn reference(&mut self) {
        let start = self.pos;
        self.advance();
        let content = self.pos;
        while self.peek().is_some_and(binding_character) {
            self.advance();
        }
        if self.pos == content {
            self.error("LEX002", start, "expected a name after '$'");
        }
        if self.text[self.pos..].starts_with("::") {
            self.pos += 2;
            while self.peek().is_some_and(name_character) {
                self.advance();
            }
        }
        while self.peek() == Some('.') {
            self.advance();
            let field = self.pos;
            while self.peek().is_some_and(name_character) {
                self.advance();
            }
            if field == self.pos {
                self.error("LEX002", field, "expected a field name after '.'");
                break;
            }
        }
        self.emit(
            TokenKind::Ref,
            self.text[content..self.pos].to_string(),
            start,
        );
    }
    fn string(&mut self) {
        let start = self.pos;
        self.advance();
        let mut value = String::new();
        while let Some(ch) = self.peek() {
            if ch == '"' || ch == '\n' {
                break;
            }
            let escape_start = self.pos;
            self.advance();
            if ch != '\\' {
                value.push(ch);
                continue;
            }
            if let Some(escaped) = self.advance() {
                let escaped = match escaped {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '"' | '\\' => escaped,
                    _ => {
                        self.error(
                            "LEX004",
                            escape_start,
                            format!("'\\{escaped}' escapes nothing"),
                        );
                        escaped
                    }
                };
                value.push(escaped);
            } else {
                value.push('\\');
            }
        }
        if self.peek() == Some('"') {
            self.advance();
        } else {
            self.error("LEX001", start, "unterminated quoted value");
        }
        self.emit(TokenKind::String, value, start);
    }
}
