use super::Package;
use crate::{Diagnostic, Span};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Word(String),
    Reference(String),
    Number(String),
    Text(String),
    Symbol(String),
    End,
}
#[derive(Clone, Debug)]
pub(super) struct Token {
    pub kind: Kind,
    pub span: Span,
}
pub(super) fn error(start: usize, end: usize, message: &str) -> Diagnostic {
    Diagnostic::error("CAL001", Span::new(start, end).expect("ordered"), message)
}

pub(super) struct Scanner<'a> {
    pub text: &'a str,
    pub pos: usize,
    pub offset: usize,
}
impl<'a> Scanner<'a> {
    pub fn new(text: &'a str, offset: usize) -> Self {
        Self {
            text,
            pos: 0,
            offset,
        }
    }
    fn peek(&self) -> Option<char> {
        self.text[self.pos..].chars().next()
    }
    fn next(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }
    fn problem(&self, start: usize, message: &str) -> Diagnostic {
        error(self.offset + start, self.offset + self.pos, message)
    }
    fn space(&mut self) -> Result<(), Diagnostic> {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.next();
            }
            if self.text[self.pos..].starts_with("//") {
                while self.peek().is_some_and(|c| c != '\n') {
                    self.next();
                }
            } else if self.text[self.pos..].starts_with("/*") {
                let start = self.pos;
                self.pos += 2;
                let Some(end) = self.text[self.pos..].find("*/") else {
                    self.pos = self.text.len();
                    return Err(self.problem(start, "unclosed comment"));
                };
                self.pos += end + 2;
            } else {
                return Ok(());
            }
        }
    }
    fn hex(&mut self, start: usize) -> Result<u32, Diagnostic> {
        let mut n = 0;
        for _ in 0..4 {
            let d = self
                .next()
                .and_then(|c| c.to_digit(16))
                .ok_or_else(|| self.problem(start, "invalid Unicode escape"))?;
            n = n * 16 + d;
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String, Diagnostic> {
        let start = self.pos;
        let quote = self.next().expect("quote");
        let mut result = String::new();
        while let Some(ch) = self.next() {
            if ch == quote {
                return Ok(result);
            }
            if ch < ' ' {
                return Err(self.problem(start, "unescaped control character in string"));
            }
            if ch != '\\' {
                result.push(ch);
                continue;
            }
            let escaped = match self.next() {
                Some('n') => '\n',
                Some('r') => '\r',
                Some('t') => '\t',
                Some('b') => '\u{8}',
                Some('f') => '\u{c}',
                Some(c @ ('\\' | '\'' | '"' | '/')) => c,
                Some('u') => {
                    let mut code = self.hex(start)?;
                    if (0xD800..=0xDBFF).contains(&code) {
                        if !self.text[self.pos..].starts_with("\\u") {
                            return Err(self.problem(start, "unpaired Unicode surrogate"));
                        }
                        self.pos += 2;
                        let low = self.hex(start)?;
                        if !(0xDC00..=0xDFFF).contains(&low) {
                            return Err(self.problem(start, "unpaired Unicode surrogate"));
                        }
                        code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                    }
                    char::from_u32(code)
                        .ok_or_else(|| self.problem(start, "invalid Unicode scalar"))?
                }
                _ => return Err(self.problem(start, "invalid string escape")),
            };
            result.push(escaped);
        }
        Err(self.problem(start, "unclosed string"))
    }
    fn token(&mut self, package: &Package) -> Result<Token, Diagnostic> {
        self.space()?;
        let start = self.pos;
        let kind = match self.peek() {
            None => Kind::End,
            Some('"' | '\'') => Kind::Text(self.string()?),
            Some('$') => {
                self.next();
                let begin = self.pos;
                let mut name = if self.peek() == Some('[') {
                    self.next();
                    self.space()?;
                    if !matches!(self.peek(), Some('"' | '\'')) {
                        return Err(
                            self.problem(start, "workspace selector requires a quoted name")
                        );
                    }
                    let name = self.string()?;
                    self.space()?;
                    if self.next() != Some(']') {
                        return Err(self.problem(start, "expected ']' after workspace name"));
                    }
                    name
                } else {
                    while self.peek().is_some_and(crate::lexer::binding_character) {
                        self.next();
                    }
                    self.text[begin..self.pos].to_owned()
                };
                if self.text[self.pos..].starts_with("::") {
                    let selector = self.pos;
                    self.pos += 2;
                    while self.peek().is_some_and(|c| c.is_ascii_alphabetic()) {
                        self.next();
                    }
                    name.push_str(&self.text[selector..self.pos]);
                }
                if name.is_empty() {
                    return Err(self.problem(start, "missing workspace reference"));
                }
                Kind::Reference(name)
            }
            Some(c) if c == '_' || c.is_alphabetic() => {
                self.next();
                while self.peek().is_some_and(|c| c == '_' || c.is_alphanumeric()) {
                    self.next();
                }
                Kind::Word(self.text[start..self.pos].into())
            }
            Some(c) if c.is_ascii_digit() => {
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.next();
                }
                if self.peek() == Some('.')
                    && self.text[self.pos + 1..]
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_digit())
                {
                    self.next();
                    while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.next();
                    }
                }
                if matches!(self.peek(), Some('e' | 'E')) {
                    self.next();
                    if matches!(self.peek(), Some('+' | '-')) {
                        self.next();
                    }
                    let before = self.pos;
                    while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        self.next();
                    }
                    if before == self.pos {
                        return Err(self.problem(start, "missing decimal exponent"));
                    }
                }
                Kind::Number(self.text[start..self.pos].into())
            }
            Some(c) if "{}()[];,:.".contains(c) => {
                self.next();
                Kind::Symbol(c.to_string())
            }
            Some(_) => {
                let tail = &self.text[self.pos..];
                let symbol = package
                    .operators()
                    .chain(["=>", "=", "!", "-", "+"])
                    .filter(|s| tail.starts_with(s))
                    .max_by_key(|s| s.len());
                let Some(symbol) = symbol else {
                    self.next();
                    return Err(self.problem(start, if tail.starts_with('?') { "the ?: operator is not supported; use if/else and return the selected value" } else { "unsupported calculation token" }));
                };
                self.pos += symbol.len();
                Kind::Symbol(symbol.into())
            }
        };
        Ok(Token {
            kind,
            span: Span::new(self.offset + start, self.offset + self.pos).expect("ordered"),
        })
    }
}
pub(super) fn lex(text: &str, offset: usize, package: &Package) -> Result<Vec<Token>, Diagnostic> {
    if text.len() > 1024 * 1024 || offset.checked_add(text.len()).is_none() {
        return Err(error(offset, offset, "calculation source exceeds limit"));
    }
    let mut scanner = Scanner::new(text, offset);
    let mut tokens = Vec::new();
    loop {
        let token = scanner.token(package)?;
        let end = token.kind == Kind::End;
        tokens.push(token);
        if tokens.len() > 100_000 {
            return Err(scanner.problem(scanner.pos, "too many calculation tokens"));
        }
        if end {
            break;
        }
    }
    Ok(tokens)
}

/// Finds a calc block without interpreting its body or consuming following output bindings.
pub fn context_end(text: &str, start: usize) -> Result<usize, Diagnostic> {
    context_bounds(text, start).map(|(_, end)| end)
}
pub(super) fn context_requires_pure(text: &str, start: usize) -> Result<bool, Diagnostic> {
    let mut scanner = Scanner::new(&text[start + 5..], start + 5);
    scanner.space()?;
    Ok(scanner.text[scanner.pos..].starts_with("pure")
        && scanner.text[scanner.pos + 4..]
            .chars()
            .next()
            .is_some_and(|c| c.is_whitespace() || c == '{' || c == '/'))
}
pub(super) fn context_bounds(text: &str, start: usize) -> Result<(usize, usize), Diagnostic> {
    if !text.get(start..).is_some_and(|s| s.starts_with(":calc")) {
        return Err(error(start, start, "expected ':calc'"));
    }
    let mut scanner = Scanner::new(&text[start + 5..], start + 5);
    scanner.space()?;
    if context_requires_pure(text, start)? {
        scanner.pos += 4;
        scanner.space()?;
    }
    if scanner.next() != Some('{') {
        return Err(scanner.problem(0, "expected a calculation block"));
    }
    let body_start = scanner.offset + scanner.pos;
    let mut depth = 1usize;
    while scanner.pos < scanner.text.len() {
        if scanner.pos > 1024 * 1024 {
            return Err(scanner.problem(0, "calculation source exceeds limit"));
        }
        scanner.space()?;
        match scanner.peek() {
            Some('"' | '\'') => {
                scanner.string()?;
            }
            Some('{') => {
                scanner.next();
                depth += 1;
                if depth > 256 {
                    return Err(scanner.problem(0, "calculation nesting exceeds 256"));
                }
            }
            Some('}') => {
                scanner.next();
                depth -= 1;
                if depth == 0 {
                    return Ok((body_start, scanner.offset + scanner.pos));
                }
            }
            Some(_) => {
                scanner.next();
            }
            None => break,
        }
    }
    Err(scanner.problem(0, "unclosed calculation block"))
}
