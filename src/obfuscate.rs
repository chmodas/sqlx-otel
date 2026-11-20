//! SQL query text obfuscator used by [`QueryTextMode::Obfuscated`](crate::QueryTextMode).
//!
//! Literal values – strings, numbers, booleans, `PostgreSQL` dollar-quoted strings – are replaced
//! with the placeholder `?`. Comments, whitespace, identifiers (quoted or otherwise), operators,
//! parameters and `NULL` are preserved verbatim.
//!
//! The obfuscator does no lexing of its own. It renders the token stream of
//! [`crate::lexer::tokenize`], which follows the lexical rules of the backend in use (see the
//! dialect table in [`crate::lexer`]). That matters for privacy, not just accuracy: the same bytes
//! mean different things on different backends – `'c:\'` is a complete literal on `PostgreSQL` and
//! `SQLite` but an unterminated one on `MySQL`; `#` starts a comment only on `MySQL`; `"…"` is a
//! string only on `MySQL` – and a lexer applying the wrong rule loses track of where literals end
//! and emits their content as if it were structure.
//!
//! Redaction per token kind:
//!
//! - [`StringLiteral`](TokenKind::StringLiteral), [`Number`](TokenKind::Number): `?`.
//! - [`Word`](TokenKind::Word): `?` when it is `true` or `false` (any case), otherwise verbatim.
//! - [`Unterminated`](TokenKind::Unterminated): `?`. A quoted identifier or comment that never
//!   closes takes the rest of the input with it, so the obfuscator fails closed instead of emitting
//!   everything after the opener verbatim. Malformed SQL does reach this code: the span is built
//!   before the statement executes.
//! - Everything else: verbatim. `MySQL` `/*! … */` bodies are executed SQL, so the lexer emits
//!   their contents as ordinary tokens and literals inside them are redacted.
//!
//! # Known mild deviations
//!
//! These trade marginal output quality for a single pass without a keyword catalogue:
//!
//! - **Signed numbers keep the sign**: `WHERE x = -1` → `WHERE x = -?`. Only the sign survives, and
//!   avoiding unary-versus-binary classification removes a class of edge cases (`THEN -1`,
//!   `LIMIT -1`, `VALUES (-1)`).
//! - **Leading-dot numerics**: `.5` → `.?` (the `.` is punctuation, the `5` a number).
//! - **String-prefix bytes survive**: `E'…'` → `E?`, `N'…'` → `N?`, `x'…'` → `x?`.
//! - **Digit-led runs are one literal**: `0xZZ`, `1e` and `123abc` all become `?`. On `MySQL` an
//!   identifier may start with a digit (`2fa_codes`), and is redacted too.
//! - **`MySQL` executable-comment versions survive**: `/*!50000 … */` keeps its five-digit version
//!   (`/*M!`: six). Any further digits, or a run too short to be a version, are a literal and
//!   become `?`, as the server reads them as one.
//!
//! # Known limitations
//!
//! The obfuscator cannot see server settings, so it assumes each backend's defaults. Changing them
//! changes what the bytes mean and can let literal content through:
//!
//! - `PostgreSQL` with `standard_conforming_strings = off` treats `\` as an escape in every string.
//! - `MySQL` with `NO_BACKSLASH_ESCAPES` does not treat `\` as an escape; with `ANSI_QUOTES`, `"…"`
//!   is an identifier.
//! - `SQLite` reads `"…"` as a string literal when no column of that name exists. The obfuscator
//!   keeps `"…"` as an identifier – the common, documented meaning – so `WHERE name = "alice"`
//!   keeps `alice`. Use `'…'` for string literals.
//! - `MySQL` `/*+ … */` optimizer hints are comments and are kept verbatim, including any quoted
//!   values inside them.
//! - A `MySQL` `/*!NNNNN … */` whose version is newer than the server, and a `MariaDB`-only `/*M!
//!   … */` sent to `MySQL`, are plain comments to the server, but the obfuscator lexes their bodies
//!   as SQL. An odd number of quotes in such a body shifts every later literal boundary. Which
//!   reading applies depends on the server's version and fork, which the client cannot see.
//! - `PostgreSQL` 14 and earlier lex a number directly followed by `E'…'` (`1E'…'`) as a number and
//!   an E-string; the obfuscator, like `PostgreSQL` 15 and later, reads `1E` as one malformed
//!   numeral. Such input is a syntax error on every version, but its span text is still recorded.
//!
//! # Invariants
//!
//! - **Length monotonicity**: `obfuscate(s, d).len() <= s.len()` for every `s` and `d`.
//! - **Idempotency**: `obfuscate(obfuscate(s, d), d) == obfuscate(s, d)`. The placeholder `?` opens
//!   no token, so a second pass finds nothing left to redact.
//! - **No panic** on any input, including unterminated literals and comments.

use crate::lexer::{Dialect, TokenKind, tokenize};

/// Replace literal values in `sql` with `?`, preserving structure (whitespace, comments,
/// identifiers, operators, `NULL`). See module docs for the full token grammar and the
/// list of known mild deviations.
pub(crate) fn obfuscate(sql: &str, dialect: Dialect) -> String {
    let mut out = String::with_capacity(sql.len());
    for token in tokenize(sql, dialect) {
        let text = &sql[token.start..token.end];
        let redact = match token.kind {
            TokenKind::StringLiteral | TokenKind::Number | TokenKind::Unterminated => true,
            TokenKind::Word => {
                text.eq_ignore_ascii_case("true") || text.eq_ignore_ascii_case("false")
            }
            _ => false,
        };
        if redact {
            out.push('?');
        } else {
            out.push_str(text);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::obfuscate;
    use crate::lexer::Dialect;

    const ALL: &[Dialect] = &[Dialect::Postgres, Dialect::MySql, Dialect::Sqlite];
    const PG: &[Dialect] = &[Dialect::Postgres];
    const MYSQL: &[Dialect] = &[Dialect::MySql];
    const SQLITE: &[Dialect] = &[Dialect::Sqlite];

    /// Assert `obfuscate(input, dialect) == expected` for each listed dialect.
    fn check_in(dialects: &[Dialect], input: &str, expected: &str) {
        for &dialect in dialects {
            assert_eq!(
                obfuscate(input, dialect),
                expected,
                "dialect: {dialect:?}, input: {input:?}"
            );
        }
    }

    /// Assert a dialect-independent expectation in all three dialects.
    fn check(input: &str, expected: &str) {
        check_in(ALL, input, expected);
    }

    // ===========================================================================
    // strings
    // ===========================================================================

    /// A plain string literal becomes `?`.
    #[test]
    fn string_simple() {
        check("'alice'", "?");
    }

    /// The empty string is still a literal.
    #[test]
    fn string_empty() {
        check("''", "?");
    }

    /// A doubled quote escapes in every dialect.
    #[test]
    fn string_doubled_quote_escape() {
        check("'it''s'", "?");
    }

    /// On `MySQL` a backslash escapes the quote, so `'it\'s'` is one literal.
    #[test]
    fn string_backslash_escape_mysql() {
        check_in(MYSQL, r"'it\'s'", "?");
    }

    /// On `PostgreSQL` and `SQLite` a backslash is ordinary, so `'it\'` ends the literal.
    #[test]
    fn string_backslash_is_literal_elsewhere() {
        check_in(&[Dialect::Postgres, Dialect::Sqlite], r"'it\'s'", "?s?");
    }

    /// Two backslashes form one literal under either rule.
    #[test]
    fn string_escaped_backslash() {
        check(r"'\\'", "?");
    }

    /// Adjacent literals are each redacted.
    #[test]
    fn string_multiple_in_expression() {
        check("'a' || 'b'", "? || ?");
    }

    /// An unterminated string is redacted to end of input.
    #[test]
    fn string_unterminated() {
        check("'unterminated", "?");
    }

    /// A bare backslash at the end of an unterminated string does not read past EOF.
    #[test]
    fn string_trailing_backslash_at_eof() {
        check("'\\", "?");
    }

    /// The `E` prefix survives as a one-character word; the E-string is redacted.
    #[test]
    fn golden_postgres_e_string_prefix() {
        check_in(PG, r"E'a\nb'", "E?");
    }

    // ===========================================================================
    // numbers (signs are preserved verbatim)
    // ===========================================================================

    /// An integer becomes `?`.
    #[test]
    fn number_integer() {
        check("42", "?");
    }

    /// Zero becomes `?`.
    #[test]
    fn number_zero() {
        check("0", "?");
    }

    /// A decimal becomes `?`.
    #[test]
    fn number_decimal() {
        check("3.14", "?");
    }

    /// A lowercase exponent stays inside the literal.
    #[test]
    fn number_exponent_lowercase() {
        check("1.5e10", "?");
    }

    /// An uppercase, signed exponent stays inside the literal.
    #[test]
    fn number_exponent_uppercase_signed() {
        check("2E-5", "?");
    }

    /// An explicit `+` exponent sign stays inside the literal.
    #[test]
    fn number_exponent_explicit_plus() {
        check("1e+3", "?");
    }

    /// An exponent marker without digits is still part of the digit-led run.
    #[test]
    fn number_exponent_without_digits_is_one_literal() {
        check("1e", "?");
    }

    /// A lowercase hex literal becomes `?`.
    #[test]
    fn number_hex_lowercase() {
        check("0xFF", "?");
    }

    /// An uppercase hex prefix becomes `?`.
    #[test]
    fn number_hex_uppercase_prefix() {
        check("0XAB", "?");
    }

    /// Malformed hex is one digit-led run, so nothing after the `0` survives.
    #[test]
    fn number_malformed_hex_is_one_literal() {
        check("0xZZ", "?");
    }

    /// A bare `0x` at end of input is one literal.
    #[test]
    fn number_hex_prefix_at_eof_is_one_literal() {
        check("0x", "?");
    }

    /// A unary minus survives; the number does not.
    #[test]
    fn number_unary_minus_preserves_sign() {
        check("WHERE x = -1", "WHERE x = -?");
    }

    /// A unary plus survives; the number does not.
    #[test]
    fn number_unary_plus_preserves_sign() {
        check("WHERE x = +1", "WHERE x = +?");
    }

    /// A binary minus between a word and a number survives.
    #[test]
    fn number_binary_minus_preserves_sign() {
        check("SELECT a-1 FROM t", "SELECT a-? FROM t");
    }

    /// Subtraction between two numbers keeps the operator.
    #[test]
    fn number_subtract_inside_parens() {
        check("SELECT (1-1)", "SELECT (?-?)");
    }

    /// A signed bound in `BETWEEN` keeps its sign.
    #[test]
    fn number_signed_in_between() {
        check("BETWEEN 1 AND -2", "BETWEEN ? AND -?");
    }

    /// A signed number after `THEN` keeps its sign.
    #[test]
    fn number_signed_after_then() {
        check("THEN -2", "THEN -?");
    }

    /// Digits inside an identifier are part of the identifier.
    #[test]
    fn number_identifier_with_trailing_digit() {
        check("col1", "col1");
    }

    /// `.5` is punctuation followed by a number.
    #[test]
    fn number_leading_dot() {
        check(".5", ".?");
    }

    // ===========================================================================
    // booleans and NULL
    // ===========================================================================

    /// `true`, `TRUE`, `True`, `false` and `FALSE` are literals.
    #[test]
    fn booleans_in_any_case() {
        for b in ["true", "TRUE", "True", "false", "FALSE"] {
            check(b, "?");
        }
    }

    /// `NULL` is not a value and is preserved in any case.
    #[test]
    fn null_preserved() {
        check("null", "null");
        check("NULL", "NULL");
    }

    /// Words that merely contain or extend `true`/`false` are identifiers.
    #[test]
    fn boolean_lookalikes_preserved() {
        for w in ["TRUE_COLUMN", "truthy", "falsey", "true$", "trueé"] {
            check(w, w);
        }
    }

    // ===========================================================================
    // quoted identifiers
    // ===========================================================================

    /// `"…"` is an identifier on `PostgreSQL` and `SQLite` and is preserved.
    #[test]
    fn ansi_quoted_identifier_preserved() {
        check_in(
            &[Dialect::Postgres, Dialect::Sqlite],
            "\"my table\"",
            "\"my table\"",
        );
        check_in(
            &[Dialect::Postgres, Dialect::Sqlite],
            "\"with\"\"quote\"",
            "\"with\"\"quote\"",
        );
    }

    /// Backtick identifiers are preserved on `MySQL` and `SQLite`.
    #[test]
    fn backtick_quoted_identifier_preserved() {
        check_in(&[Dialect::MySql, Dialect::Sqlite], "`col`", "`col`");
        check_in(
            &[Dialect::MySql, Dialect::Sqlite],
            "`with``tick`",
            "`with``tick`",
        );
    }

    /// `[…]` identifiers are preserved on `SQLite`.
    #[test]
    fn bracket_identifier_preserved() {
        check_in(SQLITE, "[my table]", "[my table]");
    }

    /// On `PostgreSQL` and `MySQL`, `[` is a subscript and its contents are lexed normally.
    #[test]
    fn bracket_subscript_redacts_contents() {
        check_in(&[Dialect::Postgres, Dialect::MySql], "a[1]['k']", "a[?][?]");
    }

    /// An identifier next to a literal: the identifier stays, the literal goes.
    #[test]
    fn quoted_identifiers_with_string_literal() {
        check_in(
            &[Dialect::Postgres, Dialect::Sqlite],
            "SELECT \"name\" FROM users WHERE \"name\" = 'alice'",
            "SELECT \"name\" FROM users WHERE \"name\" = ?",
        );
    }

    // ===========================================================================
    // dollar quotes, parameters, variables
    // ===========================================================================

    /// A dollar quote with an empty tag is redacted.
    #[test]
    fn dollar_quoted_empty_tag() {
        check_in(PG, "$$body$$", "?");
    }

    /// Quotes inside a tagged dollar quote do not end it.
    #[test]
    fn dollar_quoted_with_tag_containing_inner_quotes() {
        check_in(PG, "$tag$body with 'quotes'$tag$", "?");
    }

    /// Positional parameters are preserved.
    #[test]
    fn dollar_positional_params() {
        check_in(PG, "$1, $42", "$1, $42");
    }

    /// An unterminated dollar quote is redacted to end of input.
    #[test]
    fn dollar_quoted_unterminated() {
        check_in(PG, "$$ab", "?");
        check_in(PG, "$tag$body$ta", "?");
    }

    /// Adjacent dollar quotes are each redacted.
    #[test]
    fn dollar_quoted_adjacent() {
        check_in(PG, "$$a$$$$b$$", "??");
    }

    /// `$body` is a lone `$` and a word on `PostgreSQL`, a word or variable elsewhere.
    #[test]
    fn dollar_without_opener_preserved() {
        check("$body", "$body");
    }

    /// `SQLite` named parameters are preserved.
    #[test]
    fn sqlite_named_parameters_preserved() {
        check_in(SQLITE, ":a, @b, $c, #d, ?1", ":a, @b, $c, #d, ??");
    }

    // ===========================================================================
    // comments
    // ===========================================================================

    /// Line-comment content is preserved; the code after it is not.
    #[test]
    fn line_comment_preserves_content() {
        check("-- secret 42\nSELECT 1", "-- secret 42\nSELECT ?");
    }

    /// A line comment may run to end of input.
    #[test]
    fn line_comment_at_eof() {
        check("-- trailing", "-- trailing");
    }

    /// Block-comment content is preserved.
    #[test]
    fn block_comment_preserves_content() {
        check("/* secret 42 */ SELECT 1", "/* secret 42 */ SELECT ?");
    }

    /// An empty block comment is preserved.
    #[test]
    fn block_comment_empty() {
        check("/**/SELECT 1", "/**/SELECT ?");
    }

    /// An unterminated block comment is redacted to end of input.
    #[test]
    fn block_comment_unterminated_redacted() {
        check("/* unterminated", "?");
    }

    /// An unterminated quoted identifier is redacted to end of input.
    #[test]
    fn quoted_identifier_unterminated_redacted() {
        check("\"abc", "?");
    }

    /// `#` on `PostgreSQL` is the XOR operator, so what follows it is lexed normally.
    #[test]
    fn postgres_hash_is_an_operator() {
        check_in(PG, "SELECT 1 # 2, 'x'", "SELECT ? # ?, ?");
    }

    /// `MySQL` optimizer hints are plain comments.
    #[test]
    fn mysql_optimizer_hint_preserved() {
        check_in(
            MYSQL,
            "SELECT /*+ MAX_EXECUTION_TIME(1000) */ 1",
            "SELECT /*+ MAX_EXECUTION_TIME(1000) */ ?",
        );
    }

    // ===========================================================================
    // combined / golden cases
    // ===========================================================================

    /// An `INSERT` with inline literals keeps its structure.
    #[test]
    fn golden_insert_with_inline_literals() {
        check(
            "INSERT INTO users (name, age) VALUES ('alice', 42)",
            "INSERT INTO users (name, age) VALUES (?, ?)",
        );
    }

    /// Whitespace and newlines are left for the compactor.
    #[test]
    fn golden_preserves_whitespace_and_newlines() {
        check(
            "SELECT *\n  FROM users\n  WHERE id = 1",
            "SELECT *\n  FROM users\n  WHERE id = ?",
        );
    }

    /// Non-ASCII identifiers round-trip.
    #[test]
    fn non_ascii_identifier_preserved() {
        check("WHERE café = 'value'", "WHERE café = ?");
    }

    // ===========================================================================
    // dialect regressions: literal content leaking through a lexer desync
    // ===========================================================================

    /// A trailing backslash is a complete literal on `PostgreSQL` and `SQLite`, so the next literal
    /// must still be found where it starts (OBFUSCATE-ISSUE.md, case 1).
    #[test]
    fn regression_backslash_does_not_escape_outside_mysql() {
        check_in(
            &[Dialect::Postgres, Dialect::Sqlite],
            r"SELECT 'c:\' AS p, 'FROM leaked_value' AS q FROM real_table",
            "SELECT ? AS p, ? AS q FROM real_table",
        );
    }

    /// An apostrophe inside a `MySQL` `#` comment must not open a literal (issue case 2).
    #[test]
    fn regression_mysql_hash_comment() {
        check_in(
            MYSQL,
            "SELECT body # don't\nFROM notes WHERE body = 'x\nFROM leaked_secret y'",
            "SELECT body # don't\nFROM notes WHERE body = ?",
        );
    }

    /// `[don't]` is one `SQLite` identifier; its apostrophe must not open a literal (issue case 3).
    #[test]
    fn regression_sqlite_bracket_identifier() {
        check_in(
            SQLITE,
            "SELECT * FROM [don't] WHERE token = 'secret'",
            "SELECT * FROM [don't] WHERE token = ?",
        );
    }

    /// An unterminated `"` region is redacted to end of input in every dialect (issue case 4).
    #[test]
    fn regression_unterminated_double_quote_fails_closed() {
        check_in(
            ALL,
            "SELECT * FROM \"unclosed WHERE ssn = 'AAAA-BBBB'",
            "SELECT * FROM ?",
        );
    }

    /// An unterminated backtick identifier is redacted to end of input where backticks quote
    /// identifiers.
    #[test]
    fn regression_unterminated_backtick_fails_closed() {
        check_in(
            &[Dialect::MySql, Dialect::Sqlite],
            "SELECT * FROM `unclosed WHERE ssn = 'AAAA-BBBB'",
            "SELECT * FROM ?",
        );
    }

    /// An unterminated `SQLite` bracket identifier is redacted to end of input.
    #[test]
    fn regression_unterminated_bracket_fails_closed() {
        check_in(
            SQLITE,
            "SELECT * FROM [unclosed WHERE ssn = 'AAAA-BBBB'",
            "SELECT * FROM ?",
        );
    }

    /// An unterminated block comment is redacted to end of input in every dialect.
    #[test]
    fn regression_unterminated_block_comment_fails_closed() {
        check_in(ALL, "SELECT 1 /* oops 'secret'", "SELECT ? ?");
    }

    /// `MySQL`'s default `sql_mode` makes `"…"` a string literal, not an identifier.
    #[test]
    fn regression_mysql_double_quoted_string() {
        check_in(
            MYSQL,
            r#"SELECT * FROM t WHERE name = "alice" OR name = "it\"s""#,
            "SELECT * FROM t WHERE name = ? OR name = ?",
        );
    }

    /// `MySQL` `--` is a comment only when followed by whitespace or a control character;
    /// `a--'secret'` is `a - -'secret'`.
    #[test]
    fn regression_mysql_double_dash_needs_whitespace() {
        check_in(MYSQL, "SELECT a--'secret' FROM t", "SELECT a--? FROM t");
    }

    /// DEL (0x7F) is a control character, so `MySQL` `--\x7F` starts a comment.
    #[test]
    fn regression_mysql_double_dash_then_del_is_comment() {
        check_in(
            MYSQL,
            "SELECT 1 --\x7Fdon't\nFROM t WHERE x = 'secret'",
            "SELECT ? --\x7Fdon't\nFROM t WHERE x = ?",
        );
    }

    /// The body of a `MySQL` `/*! … */` comment is executed, so literals inside are redacted.
    #[test]
    fn regression_mysql_executable_comment() {
        check_in(
            MYSQL,
            "SELECT /*!50000 'secret' */ 1",
            "SELECT /*!50000 ? */ ?",
        );
        check_in(
            MYSQL,
            "SELECT /*M!100100 'secret' */ 1",
            "SELECT /*M!100100 ? */ ?",
        );
    }

    /// A `*/` inside a string literal does not close a `MySQL` executable comment.
    #[test]
    fn regression_mysql_executable_comment_close_inside_string() {
        check_in(
            MYSQL,
            "SELECT /*! 'a*/b' */, 'secret'",
            "SELECT /*! ? */, ?",
        );
    }

    /// A digit run too short to be an executable-comment version is a literal the server executes,
    /// so it is redacted (code review finding: `/*!12 */` emitted `12`).
    #[test]
    fn regression_mysql_short_executable_comment_version_is_a_literal() {
        check_in(MYSQL, "SELECT /*!12 */", "SELECT /*!? */");
        check_in(MYSQL, "SELECT /*M!12345 */", "SELECT /*M!12345 */");
        check_in(MYSQL, "SELECT /*M!1234 */", "SELECT /*M!? */");
    }

    /// `\v` and `\f` belong to a `PostgreSQL` E-string continuation gap, before the newline too
    /// (code review finding).
    #[test]
    fn regression_postgres_e_string_continuation_vertical_tab() {
        check_in(PG, "SELECT E'a'\x0b\n'\\' , XSECRETX , '", "SELECT E?");
        check_in(PG, "SELECT E'a'\x0c\n'\\' , XSECRETX , '", "SELECT E?");
    }

    /// An executable comment's version takes five digits (six after `/*M!`); the rest is a numeric
    /// literal.
    #[test]
    fn regression_mysql_executable_comment_version_digit_cap() {
        check_in(MYSQL, "/*!5000012345 */", "/*!50000? */");
        check_in(MYSQL, "/*M!1001001234 */", "/*M!100100? */");
    }

    /// `PostgreSQL` block comments nest, so `don't` stays inside the outer comment.
    #[test]
    fn regression_postgres_nested_block_comment() {
        check_in(
            PG,
            "/* /* */ don't */ WHERE x='secret'",
            "/* /* */ don't */ WHERE x=?",
        );
    }

    /// A `PostgreSQL` line comment ends at `\r` as well as `\n`.
    #[test]
    fn regression_postgres_line_comment_ends_at_carriage_return() {
        check_in(PG, "SELECT 1 -- c\r'secret'", "SELECT ? -- c\r?");
    }

    /// `$` continues a `PostgreSQL` identifier, so `x$a$` is a word and not a dollar quote.
    #[test]
    fn regression_postgres_dollar_inside_identifier() {
        check_in(PG, "SELECT x$a$, 'p$a$q', 'secret'", "SELECT x$a$, ?, ?");
    }

    /// Dollar quoting is `PostgreSQL`-only; `$a$` is an identifier or variable elsewhere.
    #[test]
    fn regression_dollar_quote_is_postgres_only() {
        check_in(
            &[Dialect::MySql, Dialect::Sqlite],
            "SELECT $a$ , 'p$a$q', 'secret'",
            "SELECT $a$ , ?, ?",
        );
    }

    /// Digit separators and binary/octal prefixes are numeric literals and leave no digits.
    #[test]
    fn regression_number_forms_leave_no_digits() {
        check_in(
            ALL,
            "SELECT 4111_1111_1111_1111, 0b1010, 0o17, 0x_FF, 1e5x",
            "SELECT ?, ?, ?, ?, ?",
        );
    }

    /// A `PostgreSQL` E-string continued across a newline keeps backslash escaping, so the
    /// continuation is one literal.
    #[test]
    fn regression_postgres_e_string_continuation() {
        check_in(PG, "SELECT E'a'\n'\\' , ''secret'' , '", "SELECT E?");
        check_in(PG, "SELECT E'a' -- c\n'\\' , ''secret'' , '", "SELECT E?");
    }

    /// A non-ASCII byte starts a word, so the `E` after it is not an E-string prefix.
    #[test]
    fn regression_non_ascii_word_start() {
        check_in(PG, r"SELECT éE'c:\' , 'XSECRETX'", "SELECT éE? , ?");
    }

    /// `SQLite` `$name(…)` variables swallow their parenthesised suffix, apostrophes included.
    #[test]
    fn regression_sqlite_variable_suffix() {
        check_in(
            SQLITE,
            "SELECT $a(don't), 'XSECRETX'",
            "SELECT $a(don't), ?",
        );
    }

    /// An unterminated `SQLite` `$name(` suffix is redacted to end of input.
    #[test]
    fn regression_sqlite_unterminated_variable_suffix_fails_closed() {
        check_in(SQLITE, "SELECT @a(x 'XSECRETX'", "SELECT ?");
    }

    // ===========================================================================
    // property-based tests
    // ===========================================================================

    mod proptests {
        use super::super::obfuscate;
        use crate::lexer::strategies::{
            SENTINEL, any_obfuscation_case, dialect, digit_free_token, obfuscation_case, sql_noise,
            unterminated_opener,
        };
        use crate::lexer::{Dialect, TokenKind, tokenize};
        use proptest::prelude::*;

        /// A dialect, a valid statement in it, and an unterminated opener to append.
        fn hostile_tail_case() -> impl Strategy<Value = (Dialect, String, &'static str)> {
            dialect().prop_flat_map(|d| {
                (obfuscation_case(d), unterminated_opener(d))
                    .prop_map(move |((input, _), opener)| (d, input, opener))
            })
        }

        /// A dialect and a space-joined statement of digit-free and digit-bearing tokens.
        fn digit_free_case() -> impl Strategy<Value = (Dialect, String)> {
            dialect().prop_flat_map(|d| {
                prop::collection::vec(digit_free_token(d), 0..16)
                    .prop_map(move |tokens| (d, tokens.join(" ")))
            })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            /// Algorithmic parity: for statements built from dialect-valid tokens with hostile
            /// bodies, the output equals the concatenation of each token's known obfuscation. A
            /// lexer that mis-reads any construct in any dialect desynchronises and fails this
            /// exactly, not just when a sentinel happens to leak.
            #[test]
            fn matches_reference_rendering((d, input, expected) in any_obfuscation_case()) {
                prop_assert_eq!(obfuscate(&input, d), expected, "dialect: {:?}", d);
            }

            /// No literal content survives: every generated literal carries the sentinel and no
            /// other region does, including an unterminated literal, identifier or comment left
            /// open at the end of the statement.
            #[test]
            fn no_leak_with_unterminated_tail((d, input, opener) in hostile_tail_case()) {
                let sql = format!("{input}{opener}{SENTINEL} = 'x'");
                let out = obfuscate(&sql, d);
                prop_assert!(
                    !out.contains(SENTINEL),
                    "sentinel leaked: dialect={:?} input={:?} output={:?}", d, sql, out
                );
            }

            /// No digit survives outside a `MySQL` executable-comment version, and a version is
            /// exactly five digits (or none), so no numeric literal form escapes redaction.
            #[test]
            fn no_digit_leak((d, input) in digit_free_case()) {
                let out = obfuscate(&input, d);
                for t in tokenize(&out, d) {
                    let digits = out[t.start..t.end].bytes().filter(u8::is_ascii_digit).count();
                    let allowed = if t.kind == TokenKind::ExecCommentOpen { [0, 5] } else { [0, 0] };
                    prop_assert!(
                        allowed.contains(&digits),
                        "digit leaked: dialect={:?} input={:?} output={:?}", d, input, out
                    );
                }
            }

            /// `obfuscate` never panics on arbitrary Unicode input in any dialect.
            #[test]
            fn no_panic_on_arbitrary_strings(s in any::<String>(), d in dialect()) {
                let _ = obfuscate(&s, d);
            }

            /// `obfuscate` never panics on invalid-UTF-8-derived input in any dialect.
            #[test]
            fn no_panic_on_random_bytes(
                bytes in prop::collection::vec(any::<u8>(), 0..256),
                d in dialect(),
            ) {
                let s = String::from_utf8_lossy(&bytes).into_owned();
                let _ = obfuscate(&s, d);
            }

            /// A second pass is the identity on structured statements.
            #[test]
            fn idempotent((d, input, _) in any_obfuscation_case()) {
                let once = obfuscate(&input, d);
                prop_assert_eq!(obfuscate(&once, d), once);
            }

            /// A second pass is the identity on delimiter-heavy noise, malformed SQL included.
            #[test]
            fn idempotent_on_sql_noise(s in sql_noise(), d in dialect()) {
                let once = obfuscate(&s, d);
                prop_assert_eq!(obfuscate(&once, d), once);
            }

            /// A second pass is the identity on arbitrary Unicode input.
            #[test]
            fn idempotent_on_arbitrary_strings(s in any::<String>(), d in dialect()) {
                let once = obfuscate(&s, d);
                prop_assert_eq!(obfuscate(&once, d), once);
            }

            /// Every redaction replaces at least one byte with one byte, so the output never grows,
            /// on delimiter-heavy noise and on arbitrary input.
            #[test]
            fn length_monotonic(s in sql_noise(), t in any::<String>(), d in dialect()) {
                prop_assert!(obfuscate(&s, d).len() <= s.len());
                prop_assert!(obfuscate(&t, d).len() <= t.len());
            }
        }
    }
}
