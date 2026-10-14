//! A small Rust lexer: enough of the token grammar to find macro calls and paths in source without being fooled by
//! comments, strings or char literals. It is not a parser; the scan only needs tokens, their lines and nesting.

/// One token's kind and text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    /// An identifier or keyword (a raw identifier without its `r#`).
    Ident(String),
    /// One punctuation character: `::` is two `:` tokens.
    Punct(char),
    /// A string, char or number literal, exactly as written.
    Literal(String),
}

/// A token and the line it starts on (1-based, like `line!()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// What it is.
    pub tok: Tok,
    /// Where it starts.
    pub line: u32,
}

impl Token {
    /// The token's text: an identifier's name, the punctuation character, a literal as written.
    pub fn text(&self) -> String {
        match &self.tok {
            Tok::Ident(name) | Tok::Literal(name) => name.clone(),
            Tok::Punct(c) => c.to_string(),
        }
    }

    /// Whether this is the identifier `name`.
    pub fn is_ident(&self, name: &str) -> bool {
        matches!(&self.tok, Tok::Ident(ident) if ident == name)
    }

    /// Whether this is the punctuation character `c`.
    pub fn is_punct(&self, c: char) -> bool {
        self.tok == Tok::Punct(c)
    }
}

/// The tokens of `source`, with comments (doc comments included) dropped.
pub fn tokens(source: &str) -> Vec<Token> {
    let mut lexer = Lexer { chars: source.chars().collect(), at: 0, line: 1, out: Vec::new() };
    lexer.run();
    lexer.out
}

struct Lexer {
    chars: Vec<char>,
    at: usize,
    line: u32,
    out: Vec<Token>,
}

impl Lexer {
    fn peek(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    /// Moves past one character, counting lines.
    fn bump(&mut self) {
        if self.peek(0) == Some('\n') {
            self.line += 1;
        }
        self.at += 1;
    }

    fn text_from(&self, start: usize) -> String {
        self.chars[start..self.at].iter().collect()
    }

    fn push(&mut self, tok: Tok, line: u32) {
        self.out.push(Token { tok, line });
    }

    fn run(&mut self) {
        while let Some(c) = self.peek(0) {
            let line = self.line;
            match c {
                _ if c.is_whitespace() => self.bump(),
                '/' if self.peek(1) == Some('/') => self.line_comment(),
                '/' if self.peek(1) == Some('*') => self.block_comment(),
                '"' => {
                    let start = self.at;
                    self.quoted('"');
                    self.push(Tok::Literal(self.text_from(start)), line);
                }
                '\'' => self.char_or_lifetime(),
                _ if c.is_ascii_digit() => self.number(),
                _ if c == '_' || c.is_alphabetic() => self.word(),
                _ => {
                    self.bump();
                    self.push(Tok::Punct(c), line);
                }
            }
        }
    }

    fn line_comment(&mut self) {
        while self.peek(0).is_some_and(|c| c != '\n') {
            self.bump();
        }
    }

    /// A block comment from its opening `/*` through the matching `*/`. They nest in Rust, and the opener's `*` is
    /// never the start of a closer: `/*/` opens a comment, it doesn't close one.
    fn block_comment(&mut self) {
        self.bump();
        self.bump();
        let mut depth = 1usize;
        while let Some(c) = self.peek(0) {
            match (c, self.peek(1)) {
                ('/', Some('*')) => {
                    depth += 1;
                    self.bump();
                    self.bump();
                }
                ('*', Some('/')) => {
                    self.bump();
                    self.bump();
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => self.bump(),
            }
        }
    }

    /// A string or char body after its opening quote, with backslash escapes, through the closing `quote`.
    fn quoted(&mut self, quote: char) {
        self.bump();
        while let Some(c) = self.peek(0) {
            self.bump();
            if c == '\\' {
                self.bump();
            } else if c == quote {
                return;
            }
        }
    }

    /// A raw string from its `#`s (or opening quote): `r#"..."#` ends at a quote followed by as many `#`s.
    fn raw_string(&mut self) {
        let mut hashes = 0usize;
        while self.peek(0) == Some('#') {
            hashes += 1;
            self.bump();
        }
        self.bump(); // the opening quote
        while let Some(c) = self.peek(0) {
            self.bump();
            if c == '"' && (0..hashes).all(|i| self.peek(i) == Some('#')) {
                for _ in 0..hashes {
                    self.bump();
                }
                return;
            }
        }
    }

    /// `'a'`, `'\n'` and `'\u{1F600}'` are chars; `'a` and `'static` are lifetimes (kept as identifiers).
    fn char_or_lifetime(&mut self) {
        let line = self.line;
        let start = self.at;
        let is_char = self.peek(1) == Some('\\') || self.peek(2) == Some('\'');
        if is_char {
            self.quoted('\'');
            self.push(Tok::Literal(self.text_from(start)), line);
        } else {
            self.bump();
            while self.peek(0).is_some_and(|c| c == '_' || c.is_alphanumeric()) {
                self.bump();
            }
            self.push(Tok::Ident(self.text_from(start)), line);
        }
    }

    /// Digits, suffixes and a fractional part (`1.5f32`), but not a range (`0..n`).
    fn number(&mut self) {
        let line = self.line;
        let start = self.at;
        loop {
            match self.peek(0) {
                Some(c) if c == '_' || c.is_ascii_alphanumeric() => self.bump(),
                Some('.') if self.peek(1).is_some_and(|c| c.is_ascii_digit()) => self.bump(),
                _ => break,
            }
        }
        self.push(Tok::Literal(self.text_from(start)), line);
    }

    /// An identifier, or a prefixed literal it starts: `r"..."`, `br#"..."#`, `b"..."`, `c"..."`, `b'x'`, `r#ident`.
    fn word(&mut self) {
        let line = self.line;
        let start = self.at;
        while self.peek(0).is_some_and(|c| c == '_' || c.is_alphanumeric()) {
            self.bump();
        }
        let word = self.text_from(start);
        let next = self.peek(0);
        let raw_prefix = matches!(word.as_str(), "r" | "br" | "cr");
        if raw_prefix && (next == Some('"') || (next == Some('#') && self.raw_string_follows())) {
            self.raw_string();
            self.push(Tok::Literal(self.text_from(start)), line);
        } else if matches!(word.as_str(), "b" | "c") && next == Some('"') {
            self.quoted('"');
            self.push(Tok::Literal(self.text_from(start)), line);
        } else if word == "b" && next == Some('\'') {
            self.quoted('\'');
            self.push(Tok::Literal(self.text_from(start)), line);
        } else if word == "r" && next == Some('#') {
            self.bump(); // `r#ident`: the identifier is the part after the `#`
            let ident_start = self.at;
            while self.peek(0).is_some_and(|c| c == '_' || c.is_alphanumeric()) {
                self.bump();
            }
            self.push(Tok::Ident(self.text_from(ident_start)), line);
        } else {
            self.push(Tok::Ident(word), line);
        }
    }

    /// After `r`/`br`/`cr`: whether `#`s lead to a quote (a raw string) rather than an identifier (`r#type`).
    fn raw_string_follows(&self) -> bool {
        let mut ahead = 0;
        while self.peek(ahead) == Some('#') {
            ahead += 1;
        }
        self.peek(ahead) == Some('"')
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(source: &str) -> Vec<String> {
        tokens(source).iter().map(Token::text).collect()
    }

    #[test]
    fn comments_vanish_and_lines_are_counted() {
        let toks = tokens("a // b register_op!(X)\n/* c /* nested */ register_op!(Y) */ d\n/// doc register_op!(Z)\ne");
        let found: Vec<(String, u32)> = toks.iter().map(|t| (t.text(), t.line)).collect();
        assert_eq!(found, [("a".to_owned(), 1), ("d".to_owned(), 2), ("e".to_owned(), 4)]);
    }

    #[test]
    fn block_comments_close_only_on_their_own_closer_and_nest() {
        assert_eq!(texts("/*/ register_op!(X) */ a"), ["a"]);
        assert_eq!(texts("/**/ a /***/ b"), ["a", "b"]);
        assert_eq!(texts("/* x /* y */ register_op!(Z) */ c"), ["c"]);
        assert_eq!(texts("/* /* /* deep */ */ */ d /* unterminated"), ["d"]);
    }

    #[test]
    fn strings_and_chars_are_single_literals() {
        assert_eq!(
            texts(r##"f("register_op!(X)", r#"a "quoted" b"#, b"x", '"', '\'', b'y', c"z")"##),
            [
                "f",
                "(",
                r#""register_op!(X)""#,
                ",",
                r##"r#"a "quoted" b"#"##,
                ",",
                r#"b"x""#,
                ",",
                r#"'"'"#,
                ",",
                r"'\''",
                ",",
                "b'y'",
                ",",
                r#"c"z""#,
                ")"
            ]
        );
    }

    #[test]
    fn a_string_spanning_lines_moves_the_line_count() {
        let toks = tokens("\"a\nb\"\nx");
        assert_eq!((toks[1].text(), toks[1].line), ("x".to_owned(), 3));
    }

    #[test]
    fn lifetimes_raw_identifiers_numbers_and_ranges() {
        assert_eq!(
            texts("fn f<'a>(x: &'static r#type) { 1.5f32; 0..n; 0x1F }"),
            [
                "fn", "f", "<", "'a", ">", "(", "x", ":", "&", "'static", "type", ")", "{", "1.5f32", ";", "0", ".",
                ".", "n", ";", "0x1F", "}"
            ]
        );
    }
}
