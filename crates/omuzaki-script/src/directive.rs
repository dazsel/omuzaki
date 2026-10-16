//! Finding `// omuzaki: ...` directives in a script's source: real line comments only.
//!
//! The directive is a comment, so it is found the way the language reads comments: text inside a string, a template
//! string, a character or a block comment is not one, however much it looks like it. This scans the source once for
//! line comments, tracking just enough of the lexical structure to know where it is (strings with their escapes,
//! templates with their `${ }` code, characters, nested block comments).

/// A line comment that starts with `omuzaki:`.
pub(crate) struct Directive {
    /// 1-based line of the `//`.
    pub line: u32,
    /// 1-based column of the `//`.
    pub column: u32,
    /// Whether code comes before it on its line (a trailing comment).
    pub after_code: bool,
    /// The text after `omuzaki:`.
    pub text: String,
}

/// Where the scanner is.
enum Mode {
    /// In code; `braces` counts the `{` opened since this mode began (so a template's `${` ends at its `}`).
    Code { braces: usize },
    /// In a template string (between backticks), outside its `${ }` parts.
    Template,
}

/// The characters of the source and the position in them.
struct Cursor {
    chars: Vec<char>,
    at: usize,
    line: u32,
    column: u32,
    /// Whether code has been seen on the current line.
    code_on_line: bool,
}

impl Cursor {
    fn new(source: &str) -> Self {
        Self { chars: source.chars().collect(), at: 0, line: 1, column: 1, code_on_line: false }
    }

    fn done(&self) -> bool {
        self.at >= self.chars.len()
    }

    fn here(&self) -> char {
        self.chars.get(self.at).copied().unwrap_or('\0')
    }

    fn next(&self) -> Option<char> {
        self.chars.get(self.at + 1).copied()
    }

    /// Moves over `count` characters, keeping the line and column.
    fn step(&mut self, count: usize) {
        for _ in 0..count {
            match self.chars.get(self.at) {
                Some('\n') => {
                    self.line = self.line.saturating_add(1);
                    self.column = 1;
                    self.code_on_line = false;
                }
                Some(_) => self.column = self.column.saturating_add(1),
                None => {}
            }
            self.at += 1;
        }
    }

    /// Past a block comment that starts here; they nest.
    fn skip_block_comment(&mut self) {
        let mut depth = 1_usize;
        self.step(2);
        while !self.done() && depth > 0 {
            match (self.here(), self.next()) {
                ('/', Some('*')) => {
                    depth += 1;
                    self.step(2);
                }
                ('*', Some('/')) => {
                    depth -= 1;
                    self.step(2);
                }
                _ => self.step(1),
            }
        }
    }

    /// Past a plain string that starts here. It ends at its closing quote, or at the end of the line (the language
    /// refuses one that runs on, unless the line ends in a backslash).
    fn skip_string(&mut self) {
        self.step(1);
        while !self.done() && self.here() != '"' && self.here() != '\n' {
            self.step(if self.here() == '\\' { 2 } else { 1 });
        }
        if self.here() == '"' {
            self.step(1);
        }
    }

    /// Past a character literal that starts here; one that doesn't close soon is not a literal.
    fn skip_char(&mut self) {
        let close = self.chars[self.at + 1..].iter().take(12).enumerate().find_map(|(offset, c)| {
            (*c == '\'' && !(offset > 0 && self.chars[self.at + offset] == '\\')).then_some(offset + 1)
        });
        self.step(close.map_or(1, |offset| offset + 1));
    }

    /// How many `#` follow, starting at `from`: one pass over the run, so a caller that steps past the run reads each
    /// character once.
    fn hashes_from(&self, from: usize) -> usize {
        self.chars.get(from..).map_or(0, |rest| rest.iter().take_while(|c| **c == '#').count())
    }

    /// Past a raw string that starts here (`#"..."#`, with any number of `#` on each end), if one does; otherwise past
    /// the run of `#` (they are code). Raw strings have no escapes and may span lines.
    ///
    /// Linear in the source: a run of `#` with no quote after it is stepped over whole (each of its `#` would find the
    /// same missing quote), and a quote inside the string reads only the `#` that directly follow it.
    fn skip_raw_string(&mut self) {
        self.code_on_line = true;
        let hashes = self.hashes_from(self.at);
        if self.chars.get(self.at + hashes) != Some(&'"') {
            self.step(hashes);
            return;
        }
        self.step(hashes + 1);
        while !self.done() {
            if self.here() == '"' && self.hashes_from(self.at + 1) >= hashes {
                self.step(hashes + 1);
                return;
            }
            self.step(1);
        }
    }

    /// The line comment that starts here, as a directive if it is one. The cursor ends at the end of the line.
    fn line_comment(&mut self) -> Option<Directive> {
        let (line, column, after_code) = (self.line, self.column, self.code_on_line);
        let begin = self.at + 2;
        let end = self.chars[begin..].iter().position(|c| *c == '\n').map_or(self.chars.len(), |n| begin + n);
        let text: String = self.chars[begin..end].iter().collect();
        self.step(end - self.at);
        let rest = text.trim().strip_prefix("omuzaki:")?;
        Some(Directive { line, column, after_code, text: rest.to_owned() })
    }
}

/// Every `omuzaki:` line comment in `source`, in order.
pub(crate) fn directives(source: &str) -> Vec<Directive> {
    let mut cursor = Cursor::new(source);
    let mut found = Vec::new();
    let mut modes = vec![Mode::Code { braces: 0 }];
    while !cursor.done() {
        let (here, next) = (cursor.here(), cursor.next());
        let nested = modes.len() > 1;
        match modes.last_mut() {
            Some(Mode::Template) => match (here, next) {
                ('\\', _) => cursor.step(2),
                ('`', _) => {
                    modes.pop();
                    cursor.step(1);
                }
                ('$', Some('{')) => {
                    modes.push(Mode::Code { braces: 0 });
                    cursor.step(2);
                }
                _ => cursor.step(1),
            },
            Some(Mode::Code { braces }) => match (here, next) {
                ('/', Some('/')) => found.extend(cursor.line_comment()),
                ('/', Some('*')) => cursor.skip_block_comment(),
                ('#', _) => cursor.skip_raw_string(),
                ('"', _) => {
                    cursor.code_on_line = true;
                    cursor.skip_string();
                }
                ('`', _) => {
                    cursor.code_on_line = true;
                    modes.push(Mode::Template);
                    cursor.step(1);
                }
                ('\'', _) => {
                    cursor.code_on_line = true;
                    cursor.skip_char();
                }
                ('{', _) => {
                    *braces += 1;
                    cursor.code_on_line = true;
                    cursor.step(1);
                }
                ('}', _) => {
                    if *braces == 0 && nested {
                        modes.pop();
                    } else {
                        *braces = braces.saturating_sub(1);
                    }
                    cursor.code_on_line = true;
                    cursor.step(1);
                }
                (c, _) => {
                    cursor.code_on_line |= !c.is_whitespace();
                    cursor.step(1);
                }
            },
            None => break,
        }
    }
    found
}
