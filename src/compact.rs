//! Whitespace compactor for `db.query.text` span attributes.
//!
//! Multi-line SQL written across several Rust source lines (a common readability idiom)
//! produces span attributes containing embedded `\n` and indentation runs that read poorly
//! in `OTel` exporters and trace viewers. This module collapses inter-token whitespace runs to
//! a single ASCII space and trims leading/trailing whitespace, while preserving every
//! whitespace byte that appears *inside* a SQL literal, identifier, or comment.
//!
//! The transformation is dialect-agnostic and runs unconditionally on every emitted
//! `db.query.text` value (both [`QueryTextMode::Full`] and [`QueryTextMode::Obfuscated`]). It
//! does **not** redact values – that is the job of [`crate::obfuscate::obfuscate`], which
//! continues to focus solely on suppressing literal payloads. The two passes compose at the
//! executor dispatch site: `compact_whitespace(obfuscate(sql))` for the obfuscated path,
//! `compact_whitespace(sql)` directly for the default path.
//!
//! [`QueryTextMode::Full`]: crate::QueryTextMode::Full
//! [`QueryTextMode::Obfuscated`]: crate::QueryTextMode::Obfuscated
//!
//! # Recognition order
//!
//! The state machine mirrors [`crate::obfuscate`]'s dispatch order so the two passes never
//! disagree about where a verbatim region begins or ends:
//!
//! 1. `--` line comment / `/* … */` block comment (no nesting); preserved verbatim.
//! 2. ANSI double-quoted identifiers and `MySQL` backtick-quoted identifiers (with the
//!    doubled-delimiter escape); preserved verbatim.
//! 3. `'…'` string literals (with `''` and backslash escapes); preserved verbatim.
//! 4. `PostgreSQL` `$tag$ … $tag$` dollar-quoted strings (tag may be empty), `$1`/`$2`/…
//!    positional parameters, and lone `$` punctuation; all preserved verbatim.
//! 5. ASCII whitespace (`' '`, `'\t'`, `'\n'`, `'\r'`): collapsed to a single space, with
//!    leading and trailing space suppressed.
//! 6. Anything else: copied through one Unicode scalar at a time.
//!
//! # Line-comment terminator handling
//!
//! Unlike block comments (which self-delimit with `*/`) and string/identifier literals
//! (which self-delimit with their closing quote), `--` line comments terminate at the next
//! `\n`. If compaction collapsed that `\n` to a space, `-- comment\nFROM x` would become
//! `-- comment FROM x`, sweeping `FROM x` into the comment scope. To avoid that, the line
//! comment scan consumes the terminating `\n` (when present) into the verbatim slice, and
//! the main loop sets `last_was_space = true` afterwards so any redundant whitespace that
//! follows the comment is suppressed without losing the structurally-required `\n`.
//!
//! `\r` is **not** a terminator – it is part of the comment body, mirroring
//! [`crate::obfuscate::scan_line_comment`]. CRLF inputs (`--c\r\n`) therefore preserve the
//! `\r\n` pair inside the verbatim region.
//!
//! # Invariants
//!
//! - **Length monotonicity**: `compact_whitespace(s).len() <= s.len()` for every `s`.
//! - **Idempotency**: `compact_whitespace(compact_whitespace(s)) == compact_whitespace(s)`.
//! - **Trim invariant**: the output never starts with `' '` and never ends with `' '`. It
//!   may end with `'\n'`, `'\r'`, or `'\t'` if the input ends inside a verbatim region
//!   (e.g. a line comment that runs to EOF, or a string literal whose final byte is a
//!   tab). Only the trailing-space form is structurally forbidden; whitespace-byte
//!   preservation inside verbatim regions wins for every other whitespace character.
//! - **Verbatim preservation**: bytes inside a literal, identifier, or comment region are
//!   copied byte-for-byte; any whitespace inside such a region survives.
//! - **No panic**: the implementation never panics, including on malformed UTF-8 fragments,
//!   unterminated literals, or comments that run to end of input.

/// Collapse inter-token whitespace runs in `sql` to a single space and trim leading/
/// trailing whitespace, preserving whitespace inside literals, identifiers, and comments
/// verbatim. See module docs for the full token grammar and invariants.
pub(crate) fn compact_whitespace(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    // Initialised to `true` so leading whitespace is suppressed.
    let mut last_was_space = true;

    while i < bytes.len() {
        let b = bytes[i];

        if b == b'-' && bytes.get(i + 1) == Some(&b'-') {
            i = scan_line_comment(sql, bytes, i, &mut out);
            // The line comment ends at `\n` or EOF. When the `\n` is present it has been
            // included in the verbatim slice; the next non-whitespace byte should attach
            // directly to the `\n` without an additional space.
            last_was_space = true;
        } else if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i = scan_block_comment(sql, bytes, i, &mut out);
            last_was_space = false;
        } else if b == b'"' {
            i = scan_quoted_identifier(sql, bytes, i, b'"', &mut out);
            last_was_space = false;
        } else if b == b'`' {
            i = scan_quoted_identifier(sql, bytes, i, b'`', &mut out);
            last_was_space = false;
        } else if b == b'\'' {
            i = scan_string_literal(sql, bytes, i, &mut out);
            last_was_space = false;
        } else if b == b'$' {
            i = scan_dollar(sql, bytes, i, &mut out);
            last_was_space = false;
        } else if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
            i += 1;
        } else {
            // Catch-all: copy one Unicode scalar verbatim. `sql[i..]` is on a char
            // boundary because every previous step advanced by full-token spans.
            let ch = sql[i..].chars().next().expect("i < bytes.len()");
            out.push(ch);
            i += ch.len_utf8();
            last_was_space = false;
        }
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

fn scan_line_comment(sql: &str, bytes: &[u8], start: usize, out: &mut String) -> usize {
    // Body terminates at `\n` (matching `obfuscate::scan_line_comment`). Unlike the
    // obfuscator – which lets the main loop emit the `\n` separately – the compactor
    // bundles the `\n` into the verbatim slice so the whitespace branch cannot collapse
    // the comment terminator and sweep subsequent tokens into the comment scope.
    let mut i = start + 2;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    if i < bytes.len() {
        i += 1;
    }
    out.push_str(&sql[start..i]);
    i
}

fn scan_block_comment(sql: &str, bytes: &[u8], start: usize, out: &mut String) -> usize {
    let mut i = start + 2;
    let mut closed = false;
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            i += 2;
            closed = true;
            break;
        }
        i += 1;
    }
    if !closed {
        i = bytes.len();
    }
    out.push_str(&sql[start..i]);
    i
}

fn scan_quoted_identifier(
    sql: &str,
    bytes: &[u8],
    start: usize,
    quote: u8,
    out: &mut String,
) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        if bytes[i] == quote {
            if bytes.get(i + 1) == Some(&quote) {
                i += 2;
            } else {
                i += 1;
                break;
            }
        } else {
            i += 1;
        }
    }
    out.push_str(&sql[start..i]);
    i
}

fn scan_string_literal(sql: &str, bytes: &[u8], start: usize, out: &mut String) -> usize {
    let mut i = start + 1;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' {
            // Backslash escapes the next byte (MySQL, Postgres E-strings). At EOF, a lone
            // backslash consumes nothing extra, so the scan terminates without panic.
            i += if i + 1 < bytes.len() { 2 } else { 1 };
        } else if c == b'\'' {
            if bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
            } else {
                i += 1;
                break;
            }
        } else {
            i += 1;
        }
    }
    out.push_str(&sql[start..i]);
    i
}

fn scan_dollar(sql: &str, bytes: &[u8], start: usize, out: &mut String) -> usize {
    // Postgres positional parameter: $\d+ is preserved verbatim.
    if bytes.get(start + 1).is_some_and(u8::is_ascii_digit) {
        let mut i = start + 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        out.push_str(&sql[start..i]);
        return i;
    }

    // Try to match an opening $tag$ delimiter (tag may be empty).
    let tag_start = start + 1;
    let mut tag_end = tag_start;
    if bytes
        .get(tag_end)
        .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'_')
    {
        tag_end += 1;
        while bytes
            .get(tag_end)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            tag_end += 1;
        }
    }
    if bytes.get(tag_end) != Some(&b'$') {
        // Not a dollar quote: preserve `$` as a single byte. Subsequent bytes will be
        // re-scanned by the main loop.
        out.push('$');
        return start + 1;
    }

    let tag = &bytes[tag_start..tag_end];
    let body_start = tag_end + 1;
    let mut k = body_start;
    while k < bytes.len() {
        if bytes[k] == b'$' {
            let after = k + 1;
            let close_end = after + tag.len();
            if close_end < bytes.len()
                && &bytes[after..close_end] == tag
                && bytes[close_end] == b'$'
            {
                out.push_str(&sql[start..=close_end]);
                return close_end + 1;
            }
        }
        k += 1;
    }
    // Unterminated: consume to EOF, preserving every byte verbatim.
    out.push_str(&sql[start..]);
    bytes.len()
}

#[cfg(test)]
mod tests {
    use super::compact_whitespace;

    fn check(input: &str, expected: &str) {
        assert_eq!(compact_whitespace(input), expected, "input: {input:?}");
    }

    // ===========================================================================
    // multi-line SQL collapses
    // ===========================================================================

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

    #[test]
    fn string_literal_preserves_internal_whitespace() {
        check(
            "SELECT 'hello   world\n!' FROM t",
            "SELECT 'hello   world\n!' FROM t",
        );
    }

    #[test]
    fn string_literal_doubled_quote_escape_preserved() {
        check("SELECT 'it''s' FROM t", "SELECT 'it''s' FROM t");
    }

    #[test]
    fn string_literal_backslash_escape_preserved() {
        check(r"SELECT 'it\'s' FROM t", r"SELECT 'it\'s' FROM t");
    }

    #[test]
    fn string_literal_lone_trailing_backslash_unterminated() {
        // No panic; the lone backslash consumes itself and the scan terminates at EOF.
        check("'\\", "'\\");
    }

    #[test]
    fn string_literal_unterminated_to_eof() {
        check("SELECT 'unterminated", "SELECT 'unterminated");
    }

    // ===========================================================================
    // quoted identifiers
    // ===========================================================================

    #[test]
    fn ansi_quoted_identifier_with_embedded_tab_preserved() {
        check("SELECT \"two\twords\"", "SELECT \"two\twords\"");
    }

    #[test]
    fn backtick_identifier_with_embedded_space_preserved() {
        check("SELECT `name with spaces`", "SELECT `name with spaces`");
    }

    #[test]
    fn ansi_doubled_quote_escape_preserved() {
        check("SELECT \"with\"\"quote\"", "SELECT \"with\"\"quote\"");
    }

    #[test]
    fn backtick_doubled_escape_preserved() {
        check("SELECT `with``tick`", "SELECT `with``tick`");
    }

    // ===========================================================================
    // dollar-quoted strings and positional params
    // ===========================================================================

    #[test]
    fn dollar_quoted_empty_tag_preserves_internal_indent() {
        check("SELECT $$multi\n  line$$", "SELECT $$multi\n  line$$");
    }

    #[test]
    fn dollar_quoted_with_tag_preserves_body() {
        check(
            "SELECT $tag$body  with\nnewline$tag$",
            "SELECT $tag$body  with\nnewline$tag$",
        );
    }

    #[test]
    fn dollar_positional_param_one_digit() {
        check("WHERE id = $1", "WHERE id = $1");
    }

    #[test]
    fn dollar_positional_param_multi_digit() {
        check("WHERE id = $42", "WHERE id = $42");
    }

    #[test]
    fn dollar_lone_with_no_opener_preserved() {
        // `$body` is not a dollar quote (no closing `$` of opener); `$` is preserved as
        // punctuation and `body` flows through the catch-all branch.
        check("$body", "$body");
    }

    #[test]
    fn dollar_quoted_unterminated_to_eof_no_panic() {
        check("$$ab", "$$ab");
    }

    #[test]
    fn dollar_quoted_unterminated_with_tag_to_eof() {
        check("$tag$body$ta", "$tag$body$ta");
    }

    // ===========================================================================
    // line comments
    // ===========================================================================

    #[test]
    fn line_comment_terminator_preserved_so_subsequent_tokens_stay_outside() {
        // The `\n` *inside* the comment region survives; the `\n` before `--c` collapses
        // to a single space.
        check("SELECT 1\n--c\nFROM x", "SELECT 1 --c\nFROM x");
    }

    #[test]
    fn line_comment_crlf_terminator_preserves_both_bytes() {
        // `\r` is part of the comment body (terminator is `\n` only); the `\r\n` pair
        // therefore lives inside the verbatim region.
        check("SELECT 1\n--c\r\nFROM x", "SELECT 1 --c\r\nFROM x");
    }

    #[test]
    fn line_comment_at_eof_without_newline_copied_as_is() {
        check("-- trailing", "-- trailing");
    }

    #[test]
    fn line_comment_at_eof_with_trailing_cr_preserves_cr() {
        // `\r` is part of the comment body (terminator is `\n` only). When the input
        // ends with `\r` and no `\n`, the line comment runs to EOF and the `\r` survives
        // inside the verbatim region. The trim invariant explicitly only forbids
        // trailing `' '`, so a trailing `\r` is permitted.
        check("-- trailing\r", "-- trailing\r");
    }

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

    #[test]
    fn block_comment_preserves_internal_whitespace_and_markers() {
        check(
            "/* multi\n  line */ SELECT 1",
            "/* multi\n  line */ SELECT 1",
        );
    }

    #[test]
    fn block_comment_unterminated_consumes_to_eof() {
        check("/* unterminated", "/* unterminated");
    }

    // ===========================================================================
    // whitespace collapsing semantics
    // ===========================================================================

    #[test]
    fn tabs_crlf_and_multiple_spaces_collapse() {
        check("SELECT\t\t1\r\n\r\n\r\n  FROM\tt", "SELECT 1 FROM t");
    }

    #[test]
    fn empty_input_returns_empty_string() {
        check("", "");
    }

    #[test]
    fn whitespace_only_input_returns_empty_string() {
        check("   \n\t  ", "");
    }

    #[test]
    fn leading_whitespace_suppressed() {
        check("   SELECT 1", "SELECT 1");
    }

    #[test]
    fn trailing_whitespace_trimmed() {
        check("SELECT 1   ", "SELECT 1");
    }

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

    #[test]
    fn non_ascii_identifier_preserved() {
        // Multi-byte UTF-8 bytes inside an identifier flow through the catch-all branch
        // verbatim, so the round-trip keeps the original spelling.
        check("WHERE café = 'value'", "WHERE café = 'value'");
    }

    #[test]
    fn non_ascii_identifier_in_multiline_collapses_outer_whitespace() {
        check("SELECT café\n  FROM users", "SELECT café FROM users");
    }

    // ===========================================================================
    // composition with obfuscated output (regression for the executor pipeline)
    // ===========================================================================

    #[test]
    fn post_obfuscation_multiline_collapses_with_placeholders() {
        // After obfuscate() runs, literals are already `?`. compact_whitespace must still
        // collapse the inter-token whitespace.
        let obfuscated = "SELECT ?, ?\nFROM users\nWHERE id = ?";
        check(obfuscated, "SELECT ?, ? FROM users WHERE id = ?");
    }

    // ===========================================================================
    // property-based tests
    // ===========================================================================

    mod proptests {
        // Generators below intentionally duplicate the structure of
        // `obfuscate::tests::proptests` per the design decision documented in the plan
        // (each module owns an independent state machine; no shared lexer is extracted
        // until a third consumer appears). If a token shape needs adjusting, mirror the
        // change in both modules so the two state machines continue to penetrate the
        // same input space. Quoted identifiers, string literals, dollar-quoted strings,
        // and comments are verbatim regions for compact (even though some of them are
        // preservable for obfuscate); the generators below partition the token kinds
        // accordingly.
        use super::super::compact_whitespace;
        use proptest::prelude::*;

        fn whitespace() -> impl Strategy<Value = String> {
            "[ \t\n\r]{0,5}".prop_map(String::from)
        }

        fn ident_lower() -> impl Strategy<Value = String> {
            "[a-z_][a-z0-9_]{0,7}".prop_map(String::from)
        }

        fn ansi_quoted_ident() -> impl Strategy<Value = String> {
            "[a-z0-9 _]{0,8}".prop_map(|inner| format!("\"{inner}\""))
        }

        fn backtick_quoted_ident() -> impl Strategy<Value = String> {
            "[a-z0-9 _]{0,8}".prop_map(|inner| format!("`{inner}`"))
        }

        fn safe_punct() -> impl Strategy<Value = String> {
            // Excludes characters that combine with neighbours to start tokens that would
            // change the recognition path: `-` (could form `--`), `/` (could form `/*`),
            // `'` `"` `` ` `` `$` (literal/identifier delimiters).
            prop::sample::select(vec![",", ";", "=", "<", ">", "+", "*", "(", ")", "?"])
                .prop_map(String::from)
        }

        fn integer() -> impl Strategy<Value = String> {
            "[0-9]{1,5}".prop_map(String::from)
        }

        fn string_literal_plain() -> impl Strategy<Value = String> {
            "[a-z0-9 _]{0,8}".prop_map(|inner| format!("'{inner}'"))
        }

        fn dollar_quoted_plain() -> impl Strategy<Value = String> {
            (
                "[a-z_][a-z0-9_]{0,3}".prop_map(String::from),
                "[a-z0-9 _]{0,8}".prop_map(String::from),
            )
                .prop_map(|(tag, body)| format!("${tag}${body}${tag}$"))
        }

        fn line_comment() -> impl Strategy<Value = String> {
            "[a-z0-9 _]{0,15}".prop_map(|inner| format!("--{inner}\n"))
        }

        fn block_comment() -> impl Strategy<Value = String> {
            "[a-z0-9 _]{0,15}".prop_map(|inner| format!("/*{inner}*/"))
        }

        /// Any-token generator: produces every token kind. Used for invariants that must
        /// hold over the full grammar (no-panic, idempotency, length monotonicity, trim).
        fn token_any() -> impl Strategy<Value = String> {
            prop_oneof![
                ident_lower(),
                whitespace(),
                ansi_quoted_ident(),
                backtick_quoted_ident(),
                safe_punct(),
                integer(),
                string_literal_plain(),
                dollar_quoted_plain(),
                line_comment(),
                block_comment(),
            ]
        }

        fn fragment_any() -> impl Strategy<Value = String> {
            prop::collection::vec(token_any(), 0..16).prop_map(|tokens| tokens.concat())
        }

        /// Normal-state-only fragments: identifiers, whitespace, safe punctuation,
        /// integers. Excludes every verbatim region (quoted identifiers, string literals,
        /// dollar-quoted, comments) so the output cannot legitimately contain `'\n'`,
        /// `'\r'`, or `'\t'`.
        fn token_normal_state() -> impl Strategy<Value = String> {
            prop_oneof![ident_lower(), whitespace(), safe_punct(), integer(),]
        }

        fn fragment_normal_state_only() -> impl Strategy<Value = String> {
            prop::collection::vec(token_normal_state(), 0..16).prop_map(|tokens| tokens.concat())
        }

        /// Single-verbatim-region inputs: the entire input is exactly one quoted
        /// identifier, string literal, dollar-quoted body, or block comment. The output
        /// must equal the input byte-for-byte.
        fn fragment_single_verbatim_region() -> impl Strategy<Value = String> {
            prop_oneof![
                ansi_quoted_ident(),
                backtick_quoted_ident(),
                string_literal_plain(),
                dollar_quoted_plain(),
                block_comment(),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// Never panics on arbitrary byte noise, including malformed UTF-8 fragments.
            #[test]
            fn no_panic_on_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
                let s = String::from_utf8_lossy(&bytes).into_owned();
                let _ = compact_whitespace(&s);
            }

            /// Never panics on syntactically plausible SQL fragments – penetrates the
            /// state machine where random bytes might not.
            #[test]
            fn no_panic_on_structured_fragments(s in fragment_any()) {
                let _ = compact_whitespace(&s);
            }

            /// A single space is the minimal whitespace representation and the placeholder
            /// `?` matches none of the recognised region openers; once compacted, a second
            /// pass cannot find any further whitespace runs to collapse.
            #[test]
            fn idempotent(s in fragment_any()) {
                let once = compact_whitespace(&s);
                let twice = compact_whitespace(&once);
                prop_assert_eq!(once, twice);
            }

            /// Every whitespace run shrinks to (at most) one byte and every other byte is
            /// echoed exactly once.
            #[test]
            fn length_monotonic(s in fragment_any()) {
                let out = compact_whitespace(&s);
                prop_assert!(out.len() <= s.len());
            }

            /// The output never starts with a space and never ends with a space. It may
            /// end with `'\n'` if the input ends inside a line-comment region (the `\n`
            /// is structurally required); only `' '` is forbidden as a trailing byte.
            #[test]
            fn trim_invariant(s in fragment_any()) {
                let out = compact_whitespace(&s);
                prop_assert!(!out.starts_with(' '), "leading space: {out:?}");
                prop_assert!(!out.ends_with(' '), "trailing space: {out:?}");
            }

            /// Inputs whose tokens are all normal-state cannot legitimately contain
            /// `'\n'`, `'\r'`, or `'\t'` after compaction – every whitespace byte must
            /// have collapsed to `' '`.
            #[test]
            fn normal_state_collapses_all_whitespace_controls(
                s in fragment_normal_state_only()
            ) {
                let out = compact_whitespace(&s);
                prop_assert!(
                    !out.contains('\n') && !out.contains('\r') && !out.contains('\t'),
                    "normal-state output contains whitespace control: {out:?}"
                );
            }

            /// Inputs that are a single verbatim region round-trip identically. None of
            /// the generated regions starts or ends with whitespace, so the trim step
            /// is a no-op.
            #[test]
            fn single_verbatim_region_round_trips(s in fragment_single_verbatim_region()) {
                prop_assert_eq!(compact_whitespace(&s), s);
            }
        }
    }
}
