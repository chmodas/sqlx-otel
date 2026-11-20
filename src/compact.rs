//! Whitespace compactor for `db.query.text` span attributes.
//!
//! Multi-line SQL written across several Rust source lines (a common readability idiom) produces
//! span attributes containing embedded `\n` and indentation runs that read poorly in `OTel`
//! exporters and trace viewers. This module collapses inter-token whitespace runs to a single ASCII
//! space and trims leading/trailing whitespace, while preserving every whitespace byte that
//! appears *inside* a SQL literal, identifier, or comment.
//!
//! The transformation runs on every emitted `db.query.text` value (both [`QueryTextMode::Full`] and
//! [`QueryTextMode::Obfuscated`]). It does **not** redact values – that is the job of
//! [`crate::obfuscate::obfuscate`]. The two passes compose at the executor dispatch site:
//! `compact_whitespace(obfuscate(sql, d), d)` for the obfuscated path, `compact_whitespace(sql, d)`
//! directly for the default path.
//!
//! [`QueryTextMode::Full`]: crate::QueryTextMode::Full
//! [`QueryTextMode::Obfuscated`]: crate::QueryTextMode::Obfuscated
//!
//! # Tokens
//!
//! Both passes read one token stream, [`crate::lexer::tokenize`], so they cannot disagree about
//! where a verbatim region begins or ends, and both follow the dialect of the backend in use
//! (`MySQL` `#` comments, `SQLite` `[…]` identifiers, nested `PostgreSQL` comments, and so on).
//! [`Whitespace`](TokenKind::Whitespace) tokens – runs of `' '`, `'\t'`, `'\n'` and `'\r'` – are
//! collapsed to a single space, with leading and trailing space suppressed. Every other token is
//! copied verbatim.
//!
//! # Line-comment terminator handling
//!
//! Unlike block comments (which self-delimit with `*/`) and string/identifier literals (which
//! self-delimit with their closing quote), line comments end at a line terminator. If compaction
//! collapsed that terminator to a space, `-- comment\nFROM x` would become `-- comment FROM x`,
//! sweeping `FROM x` into the comment. So when a whitespace run follows a line comment, its leading
//! terminator – `\r\n`, `\n`, or `\r` – is kept and the rest of the run is dropped.
//!
//! Which bytes terminate a line comment is the lexer's call: `\n` everywhere, and also `\r` on
//! `PostgreSQL`. On `MySQL` and `SQLite` a `\r` is part of the comment body, so `--c\r\nFROM x`
//! keeps the `\r\n` pair either way.
//!
//! # Invariants
//!
//! - **Length monotonicity**: `compact_whitespace(s, d).len() <= s.len()` for every `s`.
//! - **Idempotency**: compacting twice equals compacting once.
//! - **Trim invariant**: the output never starts with `' '` and never ends with `' '`. It may end
//!   with `'\n'`, `'\r'`, or `'\t'` if the input ends inside a verbatim region (e.g. a line comment
//!   that runs to EOF, or a string literal whose final byte is a tab).
//! - **Verbatim preservation**: bytes inside a literal, identifier, or comment region are copied
//!   byte-for-byte; any whitespace inside such a region survives.
//! - **No panic** on any input, including unterminated literals and comments.

use crate::lexer::{Dialect, TokenKind, tokenize};

/// Collapse inter-token whitespace runs in `sql` to a single space and trim leading/
/// trailing whitespace, preserving whitespace inside literals, identifiers, and comments
/// verbatim. See module docs for the full token grammar and invariants.
pub(crate) fn compact_whitespace(sql: &str, dialect: Dialect) -> String {
    let mut out = String::with_capacity(sql.len());
    // Initialised to `true` so leading whitespace is suppressed.
    let mut last_was_space = true;
    let mut after_line_comment = false;
    for token in tokenize(sql, dialect) {
        let text = &sql[token.start..token.end];
        if token.kind == TokenKind::Whitespace {
            let terminator = if after_line_comment {
                line_terminator_prefix(text)
            } else {
                ""
            };
            if !terminator.is_empty() {
                out.push_str(terminator);
                last_was_space = true;
            } else if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push_str(text);
            last_was_space = false;
        }
        after_line_comment = token.kind == TokenKind::LineComment;
    }

    // Strip every trailing space so the trim invariant holds even when the input ends
    // inside a line-comment-at-EOF region with trailing spaces inside the comment body.
    // Trailing `\n`, `\r`, or `\t` survive when they live inside a verbatim region (line
    // comment, string literal, etc.); only `' '` is structurally forbidden as a trailing
    // byte.
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// The line terminator (`"\r\n"`, `"\n"` or `"\r"`) at the start of `whitespace`, or `""` if it
/// does not start with one.
fn line_terminator_prefix(whitespace: &str) -> &str {
    if whitespace.starts_with("\r\n") {
        &whitespace[..2]
    } else if whitespace.starts_with(['\n', '\r']) {
        &whitespace[..1]
    } else {
        ""
    }
}

#[cfg(test)]
mod tests {
    use super::{compact_whitespace, line_terminator_prefix};
    use crate::lexer::Dialect;

    /// Assert `compact_whitespace(input, dialect) == expected` for each listed dialect.
    fn check_in(dialects: &[Dialect], input: &str, expected: &str) {
        for &dialect in dialects {
            assert_eq!(
                compact_whitespace(input, dialect),
                expected,
                "dialect: {dialect:?}, input: {input:?}"
            );
        }
    }

    /// Assert a dialect-independent expectation in all three dialects.
    fn check(input: &str, expected: &str) {
        check_in(
            &[Dialect::Postgres, Dialect::MySql, Dialect::Sqlite],
            input,
            expected,
        );
    }

    // ===========================================================================
    // multi-line SQL collapses
    // ===========================================================================

    /// Newlines and indentation between tokens collapse to single spaces.
    #[test]
    fn multiline_select_collapses_to_single_spaces() {
        check(
            "SELECT *\n     FROM users\n      WHERE id = 1",
            "SELECT * FROM users WHERE id = 1",
        );
    }

    // ===========================================================================
    // string literals
    // ===========================================================================

    /// Whitespace inside a string literal is verbatim.
    #[test]
    fn string_literal_preserves_internal_whitespace() {
        check(
            "SELECT 'hello   world\n!' FROM t",
            "SELECT 'hello   world\n!' FROM t",
        );
    }

    /// A doubled-quote escape stays inside the literal.
    #[test]
    fn string_literal_doubled_quote_escape_preserved() {
        check("SELECT 'it''s' FROM t", "SELECT 'it''s' FROM t");
    }

    /// A backslash before a quote never lets the literal leak into the collapsing state.
    #[test]
    fn string_literal_backslash_escape_preserved() {
        check(r"SELECT 'it\'s' FROM t", r"SELECT 'it\'s' FROM t");
    }

    /// A lone backslash at the end of an unterminated string does not panic.
    #[test]
    fn string_literal_lone_trailing_backslash_unterminated() {
        // No panic; the lone backslash consumes itself and the scan terminates at EOF.
        check("'\\", "'\\");
    }

    /// An unterminated string is verbatim to end of input.
    #[test]
    fn string_literal_unterminated_to_eof() {
        check("SELECT 'unterminated", "SELECT 'unterminated");
    }

    // ===========================================================================
    // quoted identifiers
    // ===========================================================================

    /// A tab inside a double-quoted region is verbatim.
    #[test]
    fn ansi_quoted_identifier_with_embedded_tab_preserved() {
        check("SELECT \"two\twords\"", "SELECT \"two\twords\"");
    }

    /// Spaces inside a backtick identifier survive.
    #[test]
    fn backtick_identifier_with_embedded_space_preserved() {
        check("SELECT `name with spaces`", "SELECT `name with spaces`");
    }

    /// A doubled `"` stays inside the quoted region.
    #[test]
    fn ansi_doubled_quote_escape_preserved() {
        check("SELECT \"with\"\"quote\"", "SELECT \"with\"\"quote\"");
    }

    /// A doubled backtick stays inside the quoted region.
    #[test]
    fn backtick_doubled_escape_preserved() {
        check("SELECT `with``tick`", "SELECT `with``tick`");
    }

    // ===========================================================================
    // dollar-quoted strings and positional params
    // ===========================================================================

    /// Indentation inside a `PostgreSQL` dollar quote is verbatim.
    #[test]
    fn dollar_quoted_empty_tag_preserves_internal_indent() {
        check_in(
            &[Dialect::Postgres],
            "SELECT $$multi\n  line$$",
            "SELECT $$multi\n  line$$",
        );
    }

    /// Whitespace inside a tagged dollar quote is verbatim.
    #[test]
    fn dollar_quoted_with_tag_preserves_body() {
        check_in(
            &[Dialect::Postgres],
            "SELECT $tag$body  with\nnewline$tag$",
            "SELECT $tag$body  with\nnewline$tag$",
        );
    }

    /// A one-digit positional parameter is preserved.
    #[test]
    fn dollar_positional_param_one_digit() {
        check_in(&[Dialect::Postgres], "WHERE id = $1", "WHERE id = $1");
    }

    /// A multi-digit positional parameter is preserved.
    #[test]
    fn dollar_positional_param_multi_digit() {
        check_in(&[Dialect::Postgres], "WHERE id = $42", "WHERE id = $42");
    }

    /// `$body` is not a dollar quote and is preserved.
    #[test]
    fn dollar_lone_with_no_opener_preserved() {
        // `$body` is not a dollar quote (no closing `$` of opener); `$` is preserved as
        // punctuation and `body` flows through the catch-all branch.
        check("$body", "$body");
    }

    /// An unterminated dollar quote is verbatim to end of input.
    #[test]
    fn dollar_quoted_unterminated_to_eof_no_panic() {
        check_in(&[Dialect::Postgres], "$$ab", "$$ab");
    }

    /// An unterminated tagged dollar quote is verbatim to end of input.
    #[test]
    fn dollar_quoted_unterminated_with_tag_to_eof() {
        check_in(&[Dialect::Postgres], "$tag$body$ta", "$tag$body$ta");
    }

    // ===========================================================================
    // line comments
    // ===========================================================================

    /// The newline ending a line comment survives, so the next token stays outside the comment.
    #[test]
    fn line_comment_terminator_preserved_so_subsequent_tokens_stay_outside() {
        // The `\n` *inside* the comment region survives; the `\n` before `--c` collapses
        // to a single space.
        check_in(
            &[Dialect::Postgres, Dialect::Sqlite],
            "SELECT 1\n--c\nFROM x",
            "SELECT 1 --c\nFROM x",
        );
        check("SELECT 1\n-- c\nFROM x", "SELECT 1 -- c\nFROM x");
    }

    /// On `MySQL`, `--c` is two minus signs rather than a comment, so its newline collapses.
    #[test]
    fn mysql_double_dash_without_space_is_not_a_comment() {
        check_in(
            &[Dialect::MySql],
            "SELECT 1\n--c\nFROM x",
            "SELECT 1 --c FROM x",
        );
    }

    /// A `\r\n` after a line comment survives as a pair in every dialect.
    #[test]
    fn line_comment_crlf_terminator_preserves_both_bytes() {
        // On MySQL and SQLite the `\r` is comment body and the `\n` the terminator; on
        // PostgreSQL the `\r\n` pair is the terminator. The output is the same.
        check("SELECT 1\n-- c\r\nFROM x", "SELECT 1 -- c\r\nFROM x");
    }

    /// A line comment running to end of input is copied as-is.
    #[test]
    fn line_comment_at_eof_without_newline_copied_as_is() {
        check("-- trailing", "-- trailing");
    }

    /// A trailing `\r` after a line comment survives (it is the comment body or its terminator).
    #[test]
    fn line_comment_at_eof_with_trailing_cr_preserves_cr() {
        // On MySQL and SQLite the `\r` is comment body; on PostgreSQL it is the comment's
        // terminator. Either way it survives, and the trim invariant only forbids `' '`.
        check("-- trailing\r", "-- trailing\r");
    }

    /// Trailing spaces inside a final line comment are trimmed for the trim invariant.
    #[test]
    fn line_comment_at_eof_trailing_spaces_stripped_for_trim_invariant() {
        // The trailing spaces live inside the line-comment region but the trim invariant
        // forbids any trailing `' '` in the emitted text. The comment's semantic scope
        // is unaffected (a SQL parser treats `-- trailing` and `-- trailing  ` as the
        // same comment), so the trim wins. This is the only case where a verbatim
        // region's bytes can be modified by the compactor.
        check("SELECT 1\n-- trailing  ", "SELECT 1 -- trailing");
    }

    // ===========================================================================
    // block comments
    // ===========================================================================

    /// Whitespace inside a block comment is verbatim.
    #[test]
    fn block_comment_preserves_internal_whitespace_and_markers() {
        check(
            "/* multi\n  line */ SELECT 1",
            "/* multi\n  line */ SELECT 1",
        );
    }

    /// An unterminated block comment is verbatim to end of input.
    #[test]
    fn block_comment_unterminated_consumes_to_eof() {
        check("/* unterminated", "/* unterminated");
    }

    // ===========================================================================
    // whitespace collapsing semantics
    // ===========================================================================

    /// Tabs, CRLF runs and repeated spaces collapse to one space.
    #[test]
    fn tabs_crlf_and_multiple_spaces_collapse() {
        check("SELECT\t\t1\r\n\r\n\r\n  FROM\tt", "SELECT 1 FROM t");
    }

    /// Empty input stays empty.
    #[test]
    fn empty_input_returns_empty_string() {
        check("", "");
    }

    /// Whitespace-only input compacts to the empty string.
    #[test]
    fn whitespace_only_input_returns_empty_string() {
        check("   \n\t  ", "");
    }

    /// Leading whitespace is dropped.
    #[test]
    fn leading_whitespace_suppressed() {
        check("   SELECT 1", "SELECT 1");
    }

    /// Trailing whitespace is dropped.
    #[test]
    fn trailing_whitespace_trimmed() {
        check("SELECT 1   ", "SELECT 1");
    }

    /// `?` placeholders pass through unchanged.
    #[test]
    fn placeholder_question_mark_flows_through_normal_state() {
        // `?` is not a region opener; it must traverse the catch-all branch and survive
        // unchanged so post-obfuscation strings (where literals are already `?`) round
        // through compact without further transformation.
        check(
            "SELECT ?, ? FROM t WHERE id = ?",
            "SELECT ?, ? FROM t WHERE id = ?",
        );
    }

    // ===========================================================================
    // unicode and byte-boundary safety
    // ===========================================================================

    /// Non-ASCII identifiers round-trip.
    #[test]
    fn non_ascii_identifier_preserved() {
        // Multi-byte UTF-8 bytes inside an identifier flow through the catch-all branch
        // verbatim, so the round-trip keeps the original spelling.
        check("WHERE café = 'value'", "WHERE café = 'value'");
    }

    /// Whitespace around a non-ASCII identifier still collapses.
    #[test]
    fn non_ascii_identifier_in_multiline_collapses_outer_whitespace() {
        check("SELECT café\n  FROM users", "SELECT café FROM users");
    }

    // ===========================================================================
    // composition with obfuscated output (regression for the executor pipeline)
    // ===========================================================================

    /// Obfuscated output with placeholders still collapses its whitespace.
    #[test]
    fn post_obfuscation_multiline_collapses_with_placeholders() {
        // After obfuscate() runs, literals are already `?`. compact_whitespace must still
        // collapse the inter-token whitespace.
        let obfuscated = "SELECT ?, ?\nFROM users\nWHERE id = ?";
        check(obfuscated, "SELECT ?, ? FROM users WHERE id = ?");
    }

    // ===========================================================================
    // dialect-specific regions
    // ===========================================================================

    /// `line_terminator_prefix` returns `\r\n`, `\n` or `\r` when a run starts with one, and
    /// nothing otherwise – the defensive case the lexer never produces after a line comment.
    #[test]
    fn line_terminator_prefix_cases() {
        assert_eq!(line_terminator_prefix("\r\n "), "\r\n");
        assert_eq!(line_terminator_prefix("\n\r"), "\n");
        assert_eq!(line_terminator_prefix("\r "), "\r");
        assert_eq!(line_terminator_prefix(" \n"), "");
    }

    /// The newline ending a `MySQL` `#` comment survives compaction.
    #[test]
    fn mysql_hash_comment_keeps_terminator() {
        check_in(
            &[Dialect::MySql],
            "SELECT 1 # c\n   FROM x",
            "SELECT 1 # c\nFROM x",
        );
    }

    /// Whitespace inside a `SQLite` bracket identifier is verbatim.
    #[test]
    fn sqlite_bracket_identifier_keeps_inner_whitespace() {
        check_in(&[Dialect::Sqlite], "SELECT  [a  b]", "SELECT [a  b]");
    }

    /// A nested `PostgreSQL` comment is one verbatim region.
    #[test]
    fn postgres_nested_comment_is_verbatim() {
        check_in(
            &[Dialect::Postgres],
            "SELECT /* a  /* b */  c */  1",
            "SELECT /* a  /* b */  c */ 1",
        );
    }

    /// On `PostgreSQL` a lone `\r` ends a line comment and is kept as its terminator.
    #[test]
    fn postgres_carriage_return_terminates_line_comment() {
        check_in(&[Dialect::Postgres], "--c\r  FROM x", "--c\rFROM x");
    }

    /// On `SQLite` a lone `\r` is part of the comment body, so the run after it is inside the
    /// comment and untouched.
    #[test]
    fn sqlite_carriage_return_is_comment_body() {
        check_in(&[Dialect::Sqlite], "--c\r  FROM x", "--c\r  FROM x");
    }

    /// A `MySQL` executable comment's body is ordinary SQL, so its whitespace collapses.
    #[test]
    fn mysql_executable_comment_body_collapses() {
        check_in(
            &[Dialect::MySql],
            "/*!50000   SELECT\n 1 */",
            "/*!50000 SELECT 1 */",
        );
    }

    // ===========================================================================
    // property-based tests
    // ===========================================================================

    mod proptests {
        use super::super::compact_whitespace;
        use crate::lexer::Dialect;
        use crate::lexer::strategies::{compaction_case, dialect, obfuscation_case, sql_noise};
        use proptest::prelude::*;

        /// A dialect and a compaction case for it.
        fn any_compaction_case() -> impl Strategy<Value = (Dialect, String, String)> {
            dialect().prop_flat_map(|d| compaction_case(d).prop_map(move |(i, e)| (d, i, e)))
        }

        /// A dialect and a structured statement (the obfuscation generator's input side).
        fn any_statement() -> impl Strategy<Value = (Dialect, String)> {
            dialect().prop_flat_map(|d| obfuscation_case(d).prop_map(move |(i, _)| (d, i)))
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            /// Algorithmic parity: tokens separated by arbitrary whitespace runs compact to the
            /// same tokens separated by single spaces (or the newline ending a line comment), with
            /// whitespace inside every literal, identifier and comment untouched.
            #[test]
            fn matches_reference_rendering((d, input, expected) in any_compaction_case()) {
                prop_assert_eq!(compact_whitespace(&input, d), expected, "dialect: {:?}", d);
            }

            /// Never panics on arbitrary Unicode input in any dialect.
            #[test]
            fn no_panic_on_arbitrary_strings(s in any::<String>(), d in dialect()) {
                let _ = compact_whitespace(&s, d);
            }

            /// Compacting twice equals compacting once, on structured statements.
            #[test]
            fn idempotent((d, s) in any_statement()) {
                let once = compact_whitespace(&s, d);
                prop_assert_eq!(compact_whitespace(&once, d), once);
            }

            /// Compacting twice equals compacting once, on delimiter-heavy noise and on arbitrary
            /// input.
            #[test]
            fn idempotent_on_malformed_input(s in sql_noise(), t in any::<String>(), d in dialect()) {
                for x in [s, t] {
                    let once = compact_whitespace(&x, d);
                    prop_assert_eq!(compact_whitespace(&once, d), once);
                }
            }

            /// Every whitespace run shrinks to at most its first one or two bytes and every other
            /// byte is echoed once, so the output never grows.
            #[test]
            fn length_monotonic(s in sql_noise(), t in any::<String>(), d in dialect()) {
                prop_assert!(compact_whitespace(&s, d).len() <= s.len());
                prop_assert!(compact_whitespace(&t, d).len() <= t.len());
            }

            /// The output never starts or ends with a space.
            #[test]
            fn trim_invariant(s in sql_noise(), t in any::<String>(), d in dialect()) {
                for x in [s, t] {
                    let out = compact_whitespace(&x, d);
                    prop_assert!(!out.starts_with(' '), "leading space: {:?}", out);
                    prop_assert!(!out.ends_with(' '), "trailing space: {:?}", out);
                }
            }
        }
    }
}
