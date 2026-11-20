//! Dialect-aware SQL lexer shared by [`crate::obfuscate`] and [`crate::compact`].
//!
//! Both passes must agree on where every verbatim region (comment, quoted identifier, string
//! literal) begins and ends, and both must agree with the database: a lexer that loses track of
//! where a string literal ends will emit literal content as if it were structure. So neither pass
//! scans bytes itself; each renders from the one token stream produced here.
//!
//! The lexer only finds token boundaries. It builds no parse tree and knows no keywords, and it
//! never fails: every byte of the input lands in exactly one [`Token`], and malformed input
//! (unterminated quotes or comments) produces a token that runs to end of input.
//!
//! # Dialect rules
//!
//! - **`'…'`**: a doubled `''` escapes everywhere. `\` escapes on `MySQL`, and on `PostgreSQL` only
//!   inside `E'…'`; on `SQLite` it is an ordinary character.
//! - **`"…"`**: a string literal on `MySQL` (with `""` and `\` escapes); an identifier (with `""`)
//!   on `PostgreSQL` and `SQLite`.
//! - **`` `…` ``**: an identifier (with ` `` `) on `MySQL` and `SQLite`; an operator character on
//!   `PostgreSQL`.
//! - **`[…]`**: an identifier ending at the first `]` on `SQLite`; punctuation elsewhere.
//! - **`--`**: a comment to `\n` everywhere, and also to `\r` on `PostgreSQL`. On `MySQL` only when
//!   followed by whitespace, a control character (DEL included) or end of input.
//! - **`#`**: a comment to `\n` on `MySQL`, a variable prefix on `SQLite`, an operator on
//!   `PostgreSQL`.
//! - **`/* … */`**: nests on `PostgreSQL`, flat elsewhere. On `MySQL`, `/*!` and `/*M!` open
//!   executable comments whose bodies are SQL.
//! - **`$`**: a `$1` parameter or `$tag$…$tag$` string on `PostgreSQL`, a word character on
//!   `MySQL`, a variable prefix on `SQLite`.
//!
//! A word starts with `[A-Za-z_]` or any non-ASCII byte (and on `MySQL` also `$`), and continues
//! with `[A-Za-z0-9_$]` or any non-ASCII byte. A number starts with a digit and greedily takes
//! `[0-9A-Za-z_]`, one `.`, and a sign after an exponent marker, so hex, binary, octal,
//! digit-separated and malformed numerals are all a single [`TokenKind::Number`].
//!
//! Further details, each mirroring the engine's own lexer:
//!
//! - **`PostgreSQL` E-strings.** A string uses backslash escapes when it directly follows a
//!   standalone `E`/`e` word. When an E-string is followed by whitespace (and optionally `--`
//!   comments) containing a newline and then another `'`, `PostgreSQL` continues the same literal,
//!   still with backslash escapes, so the lexer extends the token over the gap.
//! - **`MySQL` executable comments.** `/*!` opens a [`TokenKind::ExecCommentOpen`] that takes a
//!   five-digit version when five digits follow and none otherwise; `/*M!` takes six when six
//!   follow. Shorter digit runs are numeric literals, as they are to the server. The body is lexed
//!   as ordinary SQL, so a `*/` inside a string or a line comment does not close it; the first
//!   other `*/` is a [`TokenKind::ExecCommentClose`]. `/*+` hints are ordinary comments.
//! - **`SQLite` variables.** `$`, `@`, `:` and `#` followed by identifier characters (and `::`
//!   segments) form a variable, which may end in a `(…)` suffix that runs to `)` and contains no
//!   whitespace.
//!
//! # Fail-closed tokens
//!
//! An unterminated quoted identifier, block comment, or `SQLite` variable suffix becomes a
//! [`TokenKind::Unterminated`] running to end of input. An unterminated string or dollar quote is a
//! [`TokenKind::StringLiteral`] running to end of input. Either way the obfuscator redacts
//! everything after the opener rather than guessing where it should have ended.
//!
//! # Invariants
//!
//! - **Tiling**: the tokens are non-empty, contiguous, start at `0`, end at `sql.len()`, and every
//!   boundary is a `char` boundary. All delimiters are ASCII and every non-ASCII byte is consumed
//!   as part of a word or a quoted region, so no boundary can split a code point.
//! - **No panic** on any input.

#[cfg(test)]
pub(crate) mod strategies;

/// SQL dialect whose lexical rules the lexer follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dialect {
    /// `PostgreSQL`, assuming the default `standard_conforming_strings = on`.
    Postgres,
    /// `MySQL` and `MariaDB`, assuming a `sql_mode` without `ANSI_QUOTES` or `NO_BACKSLASH_ESCAPES`
    /// (the defaults, and what `sqlx` leaves in place).
    MySql,
    /// `SQLite`.
    Sqlite,
}

impl Dialect {
    /// Map a `db.system.name` value (`Database::SYSTEM`) to its dialect.
    ///
    /// The three values the crate emits – `"postgresql"`, `"mysql"`, `"sqlite"` – map to their own
    /// dialect. Anything else falls back to [`Dialect::Postgres`], the ANSI-closest rule set; that
    /// branch is unreachable in practice because `Database` is sealed.
    pub(crate) fn from_system(system: &str) -> Self {
        match system {
            "mysql" => Self::MySql,
            "sqlite" => Self::Sqlite,
            _ => Self::Postgres,
        }
    }
}

/// Lexical category of a [`Token`]. The variants are what the two renderers need to tell apart, not
/// a full SQL token taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenKind {
    /// A run of `' '`, `'\t'`, `'\n'` and `'\r'`.
    Whitespace,
    /// `--…` or `MySQL` `#…`. The span stops before the line terminator.
    LineComment,
    /// A terminated `/* … */` comment (nested comments included on `PostgreSQL`).
    BlockComment,
    /// A terminated quoted identifier: `"…"` (not `MySQL`), `` `…` `` (not `PostgreSQL`), or `[…]`
    /// (`SQLite`).
    QuotedIdent,
    /// A string literal – `'…'`, `MySQL` `"…"`, or a `PostgreSQL` dollar quote – terminated or
    /// running to end of input.
    StringLiteral,
    /// An unterminated quoted identifier, block comment, or `SQLite` variable suffix, running to
    /// end of input.
    Unterminated,
    /// `MySQL` `/*!` or `/*M!` plus its version digits.
    ExecCommentOpen,
    /// The `*/` closing a `MySQL` executable comment.
    ExecCommentClose,
    /// A `PostgreSQL` positional parameter: `$` followed by digits.
    PositionalParam,
    /// A numeric literal in any form (decimal, exponent, hex, binary, octal, digit separators).
    Number,
    /// An identifier or keyword, including `SQLite` variables such as `$name` or `:name`.
    Word,
    /// Any other single ASCII byte: operators, punctuation, `?` placeholders.
    Other,
}

/// One token: its kind and the byte range `start..end` it covers in the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token {
    /// What the bytes in `start..end` are.
    pub kind: TokenKind,
    /// Inclusive start byte offset.
    pub start: usize,
    /// Exclusive end byte offset.
    pub end: usize,
}

/// Iterator over the tokens of a SQL string. Created by [`tokenize`].
pub(crate) struct Lexer<'a> {
    /// The input as bytes; every delimiter the lexer inspects is ASCII.
    bytes: &'a [u8],
    /// Byte offset of the next token.
    pos: usize,
    /// Which dialect's rules apply.
    dialect: Dialect,
    /// `MySQL` only: the lexer is inside a `/*! … */` body, so the next top-level `*/` closes it.
    in_exec_comment: bool,
    /// `PostgreSQL` only: the previous token was a standalone `E`/`e` immediately followed by `'`,
    /// so the next token is a backslash-escaping E-string.
    e_string_next: bool,
}

/// Tokenize `sql` under the lexical rules of `dialect`.
pub(crate) fn tokenize(sql: &str, dialect: Dialect) -> Lexer<'_> {
    Lexer {
        bytes: sql.as_bytes(),
        pos: 0,
        dialect,
        in_exec_comment: false,
        e_string_next: false,
    }
}

impl Iterator for Lexer<'_> {
    type Item = Token;

    fn next(&mut self) -> Option<Token> {
        let start = self.pos;
        if start >= self.bytes.len() {
            return None;
        }
        let (kind, end) = self.scan(start);
        debug_assert!(end > start, "token must make progress");
        self.pos = end;
        Some(Token { kind, start, end })
    }
}

/// Whether `b` may continue a word in every dialect: `[A-Za-z0-9_$]` or any non-ASCII byte.
fn is_word_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

impl Lexer<'_> {
    /// The byte at `i`, or `None` past end of input.
    fn at(&self, i: usize) -> Option<u8> {
        self.bytes.get(i).copied()
    }

    /// Scan the token starting at `i` (which is in bounds) and return its kind and end offset.
    fn scan(&mut self, i: usize) -> (TokenKind, usize) {
        let b = self.bytes[i];
        let e_string = std::mem::take(&mut self.e_string_next);
        let dialect = self.dialect;
        match b {
            b' ' | b'\t' | b'\n' | b'\r' => (
                TokenKind::Whitespace,
                self.advance_while(i, |c| matches!(c, b' ' | b'\t' | b'\n' | b'\r')),
            ),
            b'-' if self.at(i + 1) == Some(b'-') && self.dash_dash_is_comment(i) => {
                (TokenKind::LineComment, self.line_end(i + 2))
            }
            b'#' if dialect == Dialect::MySql => (TokenKind::LineComment, self.line_end(i + 1)),
            b'*' if self.in_exec_comment && self.at(i + 1) == Some(b'/') => {
                self.in_exec_comment = false;
                (TokenKind::ExecCommentClose, i + 2)
            }
            b'/' if self.at(i + 1) == Some(b'*') => self.scan_block_comment(i),
            b'\'' => self.scan_string(i, b'\'', dialect == Dialect::MySql || e_string, e_string),
            b'"' if dialect == Dialect::MySql => self.scan_string(i, b'"', true, false),
            b'"' => self.scan_quoted_ident(i, b'"'),
            b'`' if dialect != Dialect::Postgres => self.scan_quoted_ident(i, b'`'),
            b'[' if dialect == Dialect::Sqlite => match self.find_byte(i + 1, b']') {
                Some(close) => (TokenKind::QuotedIdent, close + 1),
                None => (TokenKind::Unterminated, self.bytes.len()),
            },
            b'$' if dialect == Dialect::Postgres => self.scan_dollar(i),
            b'$' | b'@' | b':' | b'#' if dialect == Dialect::Sqlite => self.scan_sqlite_variable(i),
            b'0'..=b'9' => (TokenKind::Number, self.scan_number(i)),
            _ if b.is_ascii_alphabetic()
                || b == b'_'
                || b >= 0x80
                || (b == b'$' && dialect == Dialect::MySql) =>
            {
                self.scan_word(i)
            }
            // Every byte >= 0x80 is handled by the word arm, so this is one ASCII byte.
            _ => (TokenKind::Other, i + 1),
        }
    }

    /// Advance from `i` while `pred` holds; return the first offset where it does not.
    fn advance_while(&self, mut i: usize, pred: impl Fn(u8) -> bool) -> usize {
        while self.at(i).is_some_and(&pred) {
            i += 1;
        }
        i
    }

    /// Offset of the first `needle` at or after `from`.
    fn find_byte(&self, from: usize, needle: u8) -> Option<usize> {
        self.bytes
            .get(from..)?
            .iter()
            .position(|&c| c == needle)
            .map(|p| from + p)
    }

    /// Whether the `--` at `i` opens a comment. Always on `PostgreSQL` and `SQLite`; on `MySQL`
    /// only when the next byte is whitespace, a control character (including DEL), or EOF –
    /// otherwise `a--1` is `a - -1`.
    fn dash_dash_is_comment(&self, i: usize) -> bool {
        self.dialect != Dialect::MySql || self.at(i + 2).is_none_or(|c| c <= 0x20 || c == 0x7F)
    }

    /// End of a line comment whose body starts at `from`: the next `\n` (or `\r` on `PostgreSQL`),
    /// or end of input. The terminator is not included.
    fn line_end(&self, from: usize) -> usize {
        let pg = self.dialect == Dialect::Postgres;
        self.advance_while(from, |c| c != b'\n' && !(pg && c == b'\r'))
    }

    /// Scan from a `/*` at `i`: a `MySQL` executable-comment opener, a nested `PostgreSQL` comment,
    /// or a flat comment.
    fn scan_block_comment(&mut self, i: usize) -> (TokenKind, usize) {
        if self.dialect == Dialect::MySql && !self.in_exec_comment {
            let digits_start = match (self.at(i + 2), self.at(i + 3)) {
                (Some(b'!'), _) => Some(i + 3),
                (Some(b'M'), Some(b'!')) => Some(i + 4),
                _ => None,
            };
            if let Some(digits_start) = digits_start {
                let digits =
                    self.advance_while(digits_start, |c| c.is_ascii_digit()) - digits_start;
                // A version is all-or-nothing: the server takes five digits (`MariaDB`'s `/*M!`
                // also six) only when that many are present, and otherwise lexes the digits as
                // a number. Taking fewer than the server would only redact a version digit;
                // taking more would expose a literal, so the cap errs low.
                let version = match digits {
                    6.. if self.bytes[i + 2] == b'M' => 6,
                    5.. => 5,
                    _ => 0,
                };
                self.in_exec_comment = true;
                return (TokenKind::ExecCommentOpen, digits_start + version);
            }
        }

        let nested = self.dialect == Dialect::Postgres;
        let mut depth = 1usize;
        let mut j = i + 2;
        while j < self.bytes.len() {
            if self.bytes[j] == b'*' && self.at(j + 1) == Some(b'/') {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    return (TokenKind::BlockComment, j);
                }
            } else if nested && self.bytes[j] == b'/' && self.at(j + 1) == Some(b'*') {
                depth += 1;
                j += 2;
            } else {
                j += 1;
            }
        }
        (TokenKind::Unterminated, self.bytes.len())
    }

    /// Scan a string literal opened by `quote` at `i`. Doubled quotes always escape; `backslash`
    /// enables `\` escapes; `e_continuation` applies `PostgreSQL`'s rule that an E-string continues
    /// across a newline-bearing gap into the next `'…'`.
    fn scan_string(
        &self,
        i: usize,
        quote: u8,
        backslash: bool,
        e_continuation: bool,
    ) -> (TokenKind, usize) {
        let len = self.bytes.len();
        let mut j = i + 1;
        while j < len {
            let c = self.bytes[j];
            if backslash && c == b'\\' {
                // A lone backslash at EOF consumes only itself.
                j = (j + 2).min(len);
            } else if c != quote {
                j += 1;
            } else if self.at(j + 1) == Some(quote) {
                j += 2;
            } else {
                j += 1;
                match e_continuation
                    .then(|| self.e_string_continuation(j))
                    .flatten()
                {
                    Some(reopen) => j = reopen + 1,
                    None => return (TokenKind::StringLiteral, j),
                }
            }
        }
        (TokenKind::StringLiteral, len)
    }

    /// If the bytes from `from` are a `PostgreSQL` string-continuation gap – spaces, tabs, `\f`,
    /// `\v` and `--` comments with at least one newline, then `'` – return the offset of that `'`.
    /// `PostgreSQL` 16 and later accept `\v` anywhere in the gap; earlier versions reject `\v` as a
    /// syntax error, so treating it as gap whitespace can only over-redact.
    fn e_string_continuation(&self, from: usize) -> Option<usize> {
        let mut k = from;
        let mut newline = false;
        loop {
            match self.at(k)? {
                b' ' | b'\t' | 0x0B | 0x0C => k += 1,
                b'\n' | b'\r' => {
                    newline = true;
                    k += 1;
                }
                b'-' if self.at(k + 1) == Some(b'-') => k = self.line_end(k + 2),
                b'\'' if newline => return Some(k),
                _ => return None,
            }
        }
    }

    /// Scan a quoted identifier opened by `quote` at `i`, with the doubled-quote escape.
    fn scan_quoted_ident(&self, i: usize, quote: u8) -> (TokenKind, usize) {
        let mut j = i + 1;
        while let Some(close) = self.find_byte(j, quote) {
            if self.at(close + 1) == Some(quote) {
                j = close + 2;
            } else {
                return (TokenKind::QuotedIdent, close + 1);
            }
        }
        (TokenKind::Unterminated, self.bytes.len())
    }

    /// Scan a word at `i`. On `PostgreSQL`, a standalone `E`/`e` directly before `'` marks the next
    /// token as an E-string.
    fn scan_word(&mut self, i: usize) -> (TokenKind, usize) {
        let end = self.advance_while(i + 1, is_word_continue);
        if self.dialect == Dialect::Postgres
            && end == i + 1
            && matches!(self.bytes[i], b'e' | b'E')
            && self.at(end) == Some(b'\'')
        {
            self.e_string_next = true;
        }
        (TokenKind::Word, end)
    }

    /// Scan a numeric literal at `i` (a digit): `[0-9A-Za-z_]` greedily, at most one `.`, and a
    /// sign directly after `e`/`E` when a digit follows it.
    fn scan_number(&self, i: usize) -> usize {
        let mut j = i + 1;
        let mut seen_dot = false;
        loop {
            j = self.advance_while(j, |c| c.is_ascii_alphanumeric() || c == b'_');
            match self.at(j) {
                Some(b'.') if !seen_dot => {
                    seen_dot = true;
                    j += 1;
                }
                Some(b'+' | b'-')
                    if matches!(self.bytes[j - 1], b'e' | b'E')
                        && self.at(j + 1).is_some_and(|c| c.is_ascii_digit()) =>
                {
                    j += 2;
                }
                _ => return j,
            }
        }
    }

    /// Scan from a `$` at `i` on `PostgreSQL`: a positional parameter, a dollar-quoted string, or a
    /// lone `$`.
    fn scan_dollar(&self, i: usize) -> (TokenKind, usize) {
        if self.at(i + 1).is_some_and(|c| c.is_ascii_digit()) {
            return (
                TokenKind::PositionalParam,
                self.advance_while(i + 1, |c| c.is_ascii_digit()),
            );
        }
        let tag_start = i + 1;
        let tag_end = if self
            .at(tag_start)
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_' || c >= 0x80)
        {
            self.advance_while(tag_start + 1, |c| {
                c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
            })
        } else {
            tag_start
        };
        if self.at(tag_end) != Some(b'$') {
            return (TokenKind::Other, i + 1);
        }
        let delimiter = &self.bytes[i..=tag_end];
        let body = tag_end + 1;
        let end = self.bytes[body..]
            .windows(delimiter.len())
            .position(|w| w == delimiter)
            .map_or(self.bytes.len(), |p| body + p + delimiter.len());
        (TokenKind::StringLiteral, end)
    }

    /// Scan from a `$`, `@`, `:` or `#` at `i` on `SQLite`: identifier characters and `::`
    /// segments, then an optional `(…)` suffix that must close before any whitespace.
    fn scan_sqlite_variable(&self, i: usize) -> (TokenKind, usize) {
        let mut j = i + 1;
        let mut name_len = 0usize;
        loop {
            match self.at(j) {
                Some(c) if is_word_continue(c) => {
                    name_len += 1;
                    j += 1;
                }
                Some(b'(') if name_len > 0 => {
                    let end = self.advance_while(j + 1, |c| {
                        c != b')' && !matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r' | 0)
                    });
                    return if self.at(end) == Some(b')') {
                        (TokenKind::Word, end + 1)
                    } else {
                        (TokenKind::Unterminated, self.bytes.len())
                    };
                }
                Some(b':') if self.at(j + 1) == Some(b':') => j += 2,
                _ => break,
            }
        }
        if name_len == 0 {
            (TokenKind::Other, i + 1)
        } else {
            (TokenKind::Word, j)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Dialect, TokenKind, tokenize};
    use TokenKind::{
        BlockComment, ExecCommentClose, ExecCommentOpen, LineComment, Number, Other,
        PositionalParam, QuotedIdent, StringLiteral, Unterminated, Whitespace, Word,
    };

    /// Tokenize and return `(kind, text)` pairs.
    fn lex(sql: &str, dialect: Dialect) -> Vec<(TokenKind, &str)> {
        tokenize(sql, dialect)
            .map(|t| (t.kind, &sql[t.start..t.end]))
            .collect()
    }

    /// Assert the token stream of `sql` under `dialect`.
    fn check(dialect: Dialect, sql: &str, expected: &[(TokenKind, &str)]) {
        assert_eq!(
            lex(sql, dialect),
            expected,
            "dialect: {dialect:?}, sql: {sql:?}"
        );
    }

    const ALL: [Dialect; 3] = [Dialect::Postgres, Dialect::MySql, Dialect::Sqlite];

    /// `from_system` maps every `Database::SYSTEM` value to its own dialect and anything else to
    /// the ANSI-closest fallback.
    #[test]
    fn from_system_maps_each_backend() {
        assert_eq!(Dialect::from_system("postgresql"), Dialect::Postgres);
        assert_eq!(Dialect::from_system("mysql"), Dialect::MySql);
        assert_eq!(Dialect::from_system("sqlite"), Dialect::Sqlite);
        assert_eq!(Dialect::from_system("other"), Dialect::Postgres);
    }

    /// The `SYSTEM` constant of each enabled backend maps to the matching dialect.
    #[test]
    fn from_system_matches_database_constants() {
        use crate::database::Database;
        #[cfg(feature = "postgres")]
        assert_eq!(
            Dialect::from_system(sqlx::Postgres::SYSTEM),
            Dialect::Postgres
        );
        #[cfg(feature = "mysql")]
        assert_eq!(Dialect::from_system(sqlx::MySql::SYSTEM), Dialect::MySql);
        #[cfg(feature = "sqlite")]
        assert_eq!(Dialect::from_system(sqlx::Sqlite::SYSTEM), Dialect::Sqlite);
    }

    /// Empty input yields no tokens.
    #[test]
    fn empty_input_has_no_tokens() {
        for d in ALL {
            check(d, "", &[]);
        }
    }

    // ---------------------------------------------------------------------------------------
    // single-quoted strings
    // ---------------------------------------------------------------------------------------

    /// Backslash escapes inside `'…'` on `MySQL` only.
    #[test]
    fn single_quote_backslash_is_dialect_specific() {
        check(Dialect::MySql, r"'a\'b'", &[(StringLiteral, r"'a\'b'")]);
        for d in [Dialect::Postgres, Dialect::Sqlite] {
            check(
                d,
                r"'a\'b'",
                &[(StringLiteral, r"'a\'"), (Word, "b"), (StringLiteral, "'")],
            );
        }
    }

    /// The doubled-quote escape works in every dialect.
    #[test]
    fn single_quote_doubled_escape() {
        for d in ALL {
            check(d, "'it''s'", &[(StringLiteral, "'it''s'")]);
        }
    }

    /// A lone backslash at end of an unterminated `MySQL` string does not read past EOF.
    #[test]
    fn single_quote_trailing_backslash_at_eof() {
        check(Dialect::MySql, "'\\", &[(StringLiteral, "'\\")]);
    }

    /// A standalone `E` before `'` opens a backslash-escaping string on `PostgreSQL`.
    #[test]
    fn postgres_e_string() {
        check(
            Dialect::Postgres,
            r"e'a\'b'",
            &[(Word, "e"), (StringLiteral, r"'a\'b'")],
        );
        check(
            Dialect::Postgres,
            r"somE'a\'",
            &[(Word, "somE"), (StringLiteral, r"'a\'")],
        );
    }

    /// `E` is only an E-string prefix on `PostgreSQL`.
    #[test]
    fn e_prefix_is_postgres_only() {
        check(
            Dialect::Sqlite,
            r"E'a\'",
            &[(Word, "E"), (StringLiteral, r"'a\'")],
        );
    }

    /// An E-string continues across a newline-bearing gap; a standard string does not.
    #[test]
    fn postgres_e_string_continuation() {
        check(
            Dialect::Postgres,
            "E'a'\n 'b\\''",
            &[(Word, "E"), (StringLiteral, "'a'\n 'b\\''")],
        );
        check(
            Dialect::Postgres,
            "'a'\n'b'",
            &[
                (StringLiteral, "'a'"),
                (Whitespace, "\n"),
                (StringLiteral, "'b'"),
            ],
        );
    }

    /// The continuation gap needs a newline and may include `--` comments, `\v` and `\f`; without a
    /// newline, or with anything else, the E-string ends.
    #[test]
    fn postgres_e_string_continuation_gap_rules() {
        check(
            Dialect::Postgres,
            "E'a' -- c\n\x0b'b'",
            &[(Word, "E"), (StringLiteral, "'a' -- c\n\x0b'b'")],
        );
        check(
            Dialect::Postgres,
            "E'a' 'b'",
            &[
                (Word, "E"),
                (StringLiteral, "'a'"),
                (Whitespace, " "),
                (StringLiteral, "'b'"),
            ],
        );
        check(
            Dialect::Postgres,
            "E'a'\x0b\x0c\n'b'",
            &[(Word, "E"), (StringLiteral, "'a'\x0b\x0c\n'b'")],
        );
        check(
            Dialect::Postgres,
            "E'a'\nx",
            &[
                (Word, "E"),
                (StringLiteral, "'a'"),
                (Whitespace, "\n"),
                (Word, "x"),
            ],
        );
        check(
            Dialect::Postgres,
            "E'a'\n",
            &[(Word, "E"), (StringLiteral, "'a'"), (Whitespace, "\n")],
        );
    }

    // ---------------------------------------------------------------------------------------
    // double quotes, backticks, brackets
    // ---------------------------------------------------------------------------------------

    /// `"…"` is an identifier on `PostgreSQL` and `SQLite`, a backslash-escaping string on `MySQL`.
    #[test]
    fn double_quote_by_dialect() {
        for d in [Dialect::Postgres, Dialect::Sqlite] {
            check(d, "\"a\"\"b\"", &[(QuotedIdent, "\"a\"\"b\"")]);
        }
        check(
            Dialect::MySql,
            r#""a\"b""c""#,
            &[(StringLiteral, r#""a\"b""c""#)],
        );
    }

    /// Backticks quote identifiers on `MySQL` and `SQLite`; on `PostgreSQL` they are operator
    /// characters.
    #[test]
    fn backtick_by_dialect() {
        for d in [Dialect::MySql, Dialect::Sqlite] {
            check(d, "`a``b`", &[(QuotedIdent, "`a``b`")]);
        }
        check(
            Dialect::Postgres,
            "`a`",
            &[(Other, "`"), (Word, "a"), (Other, "`")],
        );
    }

    /// `[…]` is a `SQLite` identifier ending at the first `]`; elsewhere `[` is punctuation.
    #[test]
    fn bracket_by_dialect() {
        check(
            Dialect::Sqlite,
            "[a'b]]",
            &[(QuotedIdent, "[a'b]"), (Other, "]")],
        );
        for d in [Dialect::Postgres, Dialect::MySql] {
            check(
                d,
                "a[1]",
                &[(Word, "a"), (Other, "["), (Number, "1"), (Other, "]")],
            );
        }
    }

    /// Unterminated quoted identifiers run to end of input as `Unterminated`.
    #[test]
    fn unterminated_identifiers() {
        check(Dialect::Postgres, "\"a\"\"b", &[(Unterminated, "\"a\"\"b")]);
        check(Dialect::MySql, "`a", &[(Unterminated, "`a")]);
        check(Dialect::Sqlite, "[a", &[(Unterminated, "[a")]);
    }

    // ---------------------------------------------------------------------------------------
    // comments
    // ---------------------------------------------------------------------------------------

    /// `--` ends at `\n` everywhere and also at `\r` on `PostgreSQL`.
    #[test]
    fn line_comment_terminators() {
        check(
            Dialect::Postgres,
            "--a\rb",
            &[(LineComment, "--a"), (Whitespace, "\r"), (Word, "b")],
        );
        for d in [Dialect::MySql, Dialect::Sqlite] {
            check(
                d,
                "-- a\rb\nc",
                &[(LineComment, "-- a\rb"), (Whitespace, "\n"), (Word, "c")],
            );
        }
    }

    /// `MySQL` `--` needs whitespace, a control character, or EOF after it.
    #[test]
    fn mysql_double_dash_rule() {
        check(Dialect::MySql, "--", &[(LineComment, "--")]);
        check(Dialect::MySql, "--\t", &[(LineComment, "--\t")]);
        check(Dialect::MySql, "--\x7f", &[(LineComment, "--\x7f")]);
        check(
            Dialect::MySql,
            "--1",
            &[(Other, "-"), (Other, "-"), (Number, "1")],
        );
        check(Dialect::Postgres, "--1", &[(LineComment, "--1")]);
    }

    /// `#` is a comment on `MySQL`, an operator on `PostgreSQL`, a variable prefix on `SQLite`.
    #[test]
    fn hash_by_dialect() {
        check(Dialect::MySql, "#a'b", &[(LineComment, "#a'b")]);
        check(Dialect::Postgres, "#a", &[(Other, "#"), (Word, "a")]);
        check(Dialect::Sqlite, "#a", &[(Word, "#a")]);
    }

    /// Block comments nest on `PostgreSQL` only.
    #[test]
    fn block_comment_nesting() {
        check(Dialect::Postgres, "/*/**/*/", &[(BlockComment, "/*/**/*/")]);
        for d in [Dialect::MySql, Dialect::Sqlite] {
            check(d, "/*/**/x", &[(BlockComment, "/*/**/"), (Word, "x")]);
        }
    }

    /// `*/` takes precedence over `/*` inside a `PostgreSQL` comment, and `**/` closes.
    #[test]
    fn postgres_comment_close_precedence() {
        check(
            Dialect::Postgres,
            "/* */*/",
            &[(BlockComment, "/* */"), (Other, "*"), (Other, "/")],
        );
        check(Dialect::Postgres, "/***/", &[(BlockComment, "/***/")]);
    }

    /// Unterminated block comments run to end of input as `Unterminated`.
    #[test]
    fn unterminated_block_comment() {
        for d in ALL {
            check(d, "/* a", &[(Unterminated, "/* a")]);
        }
        check(Dialect::Postgres, "/* /* */", &[(Unterminated, "/* /* */")]);
    }

    /// `MySQL` executable comments: version digits are capped, the body is ordinary SQL, and the
    /// first top-level `*/` closes it.
    #[test]
    fn mysql_executable_comment() {
        check(
            Dialect::MySql,
            "/*!12 */",
            &[
                (ExecCommentOpen, "/*!"),
                (Number, "12"),
                (Whitespace, " "),
                (ExecCommentClose, "*/"),
            ],
        );
        check(
            Dialect::MySql,
            "/*!123456*/",
            &[
                (ExecCommentOpen, "/*!12345"),
                (Number, "6"),
                (ExecCommentClose, "*/"),
            ],
        );
        check(
            Dialect::MySql,
            "/*M!12345 */",
            &[
                (ExecCommentOpen, "/*M!12345"),
                (Whitespace, " "),
                (ExecCommentClose, "*/"),
            ],
        );
        check(
            Dialect::MySql,
            "/*!12345678*/",
            &[
                (ExecCommentOpen, "/*!12345"),
                (Number, "678"),
                (ExecCommentClose, "*/"),
            ],
        );
        check(
            Dialect::MySql,
            "/*M!1234567 */",
            &[
                (ExecCommentOpen, "/*M!123456"),
                (Number, "7"),
                (Whitespace, " "),
                (ExecCommentClose, "*/"),
            ],
        );
        check(
            Dialect::MySql,
            "/*!'*/'#*/\n/*x*/*/*/",
            &[
                (ExecCommentOpen, "/*!"),
                (StringLiteral, "'*/'"),
                (LineComment, "#*/"),
                (Whitespace, "\n"),
                (BlockComment, "/*x*/"),
                (ExecCommentClose, "*/"),
                (Other, "*"),
                (Other, "/"),
            ],
        );
    }

    /// `/*+` hints, `/*M` without `!`, and `/*!` on other dialects are plain comments.
    #[test]
    fn non_executable_comments() {
        check(Dialect::MySql, "/*+ a */", &[(BlockComment, "/*+ a */")]);
        check(Dialect::MySql, "/*Mx*/", &[(BlockComment, "/*Mx*/")]);
        check(Dialect::Postgres, "/*!1 */", &[(BlockComment, "/*!1 */")]);
    }

    // ---------------------------------------------------------------------------------------
    // dollar, variables, words, numbers
    // ---------------------------------------------------------------------------------------

    /// `PostgreSQL` dollar handling: parameters, quotes with ASCII and non-ASCII tags, unterminated
    /// quotes, and a lone `$`.
    #[test]
    fn postgres_dollar() {
        check(
            Dialect::Postgres,
            "$12x",
            &[(PositionalParam, "$12"), (Word, "x")],
        );
        check(Dialect::Postgres, "$$a$b$$", &[(StringLiteral, "$$a$b$$")]);
        check(Dialect::Postgres, "$é$x$é$", &[(StringLiteral, "$é$x$é$")]);
        check(Dialect::Postgres, "$t$x$ta", &[(StringLiteral, "$t$x$ta")]);
        check(Dialect::Postgres, "$a", &[(Other, "$"), (Word, "a")]);
        check(Dialect::Postgres, "x$a$", &[(Word, "x$a$")]);
    }

    /// `$` is a word character on `MySQL` and a variable prefix on `SQLite`.
    #[test]
    fn dollar_outside_postgres() {
        check(
            Dialect::MySql,
            "$a$ $1",
            &[(Word, "$a$"), (Whitespace, " "), (Word, "$1")],
        );
        check(Dialect::Sqlite, "$a$", &[(Word, "$a$")]);
    }

    /// `SQLite` variables: `::` segments, a `(…)` suffix, an unterminated suffix, and a prefix with
    /// no name.
    #[test]
    fn sqlite_variables() {
        check(
            Dialect::Sqlite,
            ":a::b(x'y)z",
            &[(Word, ":a::b(x'y)"), (Word, "z")],
        );
        check(Dialect::Sqlite, "@a(x y)", &[(Unterminated, "@a(x y)")]);
        check(Dialect::Sqlite, "@(", &[(Other, "@"), (Other, "(")]);
        check(
            Dialect::Sqlite,
            "$::",
            &[(Other, "$"), (Other, ":"), (Other, ":")],
        );
    }

    /// Words start with a letter, `_` or a non-ASCII byte, and continue with `$` and digits.
    #[test]
    fn words() {
        for d in ALL {
            check(
                d,
                "éa_1$ b",
                &[(Word, "éa_1$"), (Whitespace, " "), (Word, "b")],
            );
        }
    }

    /// Numbers greedily absorb letters, separators, one dot, and an exponent sign.
    #[test]
    fn numbers() {
        for d in ALL {
            check(d, "1_000.5e-3x", &[(Number, "1_000.5e-3x")]);
            check(
                d,
                "0b1 0o7 0xZZ 1e",
                &[
                    (Number, "0b1"),
                    (Whitespace, " "),
                    (Number, "0o7"),
                    (Whitespace, " "),
                    (Number, "0xZZ"),
                    (Whitespace, " "),
                    (Number, "1e"),
                ],
            );
            check(d, "1.2.3", &[(Number, "1.2"), (Other, "."), (Number, "3")]);
            check(d, "1e+x", &[(Number, "1e"), (Other, "+"), (Word, "x")]);
            check(d, "1-2", &[(Number, "1"), (Other, "-"), (Number, "2")]);
        }
    }

    mod proptests {
        use super::super::strategies::sql_noise;
        use super::super::{Dialect, TokenKind, tokenize};
        use proptest::prelude::*;

        /// Any of the three dialects.
        fn dialect() -> impl Strategy<Value = Dialect> {
            prop_oneof![
                Just(Dialect::Postgres),
                Just(Dialect::MySql),
                Just(Dialect::Sqlite)
            ]
        }

        /// Assert that the tokens tile `sql` exactly on char boundaries.
        fn assert_tiles(sql: &str, dialect: Dialect) -> Result<(), TestCaseError> {
            let mut pos = 0;
            for t in tokenize(sql, dialect) {
                prop_assert_eq!(t.start, pos, "gap or overlap in {:?}", sql);
                prop_assert!(t.end > t.start, "empty token in {:?}", sql);
                prop_assert!(sql.is_char_boundary(t.end), "split char in {:?}", sql);
                pos = t.end;
            }
            prop_assert_eq!(pos, sql.len());
            Ok(())
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            /// Tokens tile arbitrary Unicode input in every dialect (no panic, no gap).
            #[test]
            fn tiles_arbitrary_strings(sql in any::<String>(), d in dialect()) {
                assert_tiles(&sql, d)?;
            }

            /// Tokens tile delimiter-heavy input in every dialect.
            #[test]
            fn tiles_sql_noise(sql in sql_noise(), d in dialect()) {
                assert_tiles(&sql, d)?;
            }

            /// Executable-comment closes never outnumber opens, and opens and closes alternate.
            #[test]
            fn mysql_exec_comments_alternate(sql in sql_noise()) {
                let mut open = false;
                for t in tokenize(&sql, Dialect::MySql) {
                    match t.kind {
                        TokenKind::ExecCommentOpen => {
                            prop_assert!(!open);
                            open = true;
                        }
                        TokenKind::ExecCommentClose => {
                            prop_assert!(open);
                            open = false;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}
