//! SQL query text obfuscator used by [`QueryTextMode::Obfuscated`](crate::QueryTextMode).
//!
//! A lightweight, dialect-agnostic state machine walks the input once and replaces literal
//! values – string, numeric, hex, boolean, dollar-quoted – with the placeholder `?`.
//! Comments, whitespace, identifiers (quoted or otherwise), operators, and `NULL` are
//! preserved verbatim.
//!
//! No SQL parser is constructed; only lexical token boundaries are tracked. Recognition
//! order at each input byte (first match wins):
//!
//! 1. `--` line comment / `/* … */` block comment (no nesting); preserved verbatim.
//! 2. ANSI double-quoted identifiers and `MySQL` backtick-quoted identifiers (with the
//!    doubled-delimiter escape, e.g. `""` inside an ANSI identifier); preserved verbatim.
//! 3. `'…'` string literals (with `''` and backslash escapes); replaced with `?`.
//! 4. `PostgreSQL` `$tag$ … $tag$` dollar-quoted strings (tag may be empty); replaced.
//! 5. `PostgreSQL` positional parameters `$1`, `$2`, …; preserved verbatim.
//! 6. `0x` / `0X` followed by ≥1 hex digit; replaced with `?`.
//! 7. Decimal numeric literal (digits, optional fractional part, optional exponent);
//!    replaced with `?`.
//! 8. `[A-Za-z_][A-Za-z0-9_]*` identifier; if its lowercased form is `true` or `false`
//!    it is replaced with `?`, otherwise preserved verbatim (covers `NULL`).
//! 9. Anything else: copied through one Unicode scalar at a time.
//!
//! UTF-8 safe: every delimiter the state machine inspects is ASCII, and ASCII bytes never
//! appear inside a multibyte UTF-8 sequence, so byte-level scanning cannot split a code
//! point. Non-ASCII bytes flow through the catch-all branch and are preserved verbatim.
//!
//! # Known mild deviations
//!
//! These are intentional compromises that trade marginal output quality for a single-pass
//! implementation without a keyword catalogue:
//!
//! - **Signed numbers preserve the sign**: `-1` → `-?`, `WHERE x = -1` → `WHERE x = -?`.
//!   The sensitive payload is suppressed; only the sign survives, and a sign is not a
//!   value. Avoiding unary-vs-binary classification removes a class of edge cases
//!   (`THEN -1`, `LIMIT -1`, `RETURNING -1`, `VALUES (-1)`, `SET col = -1`).
//! - **Leading-dot numerics**: `.5` → `.?` (the `.` is treated as punctuation, the `5`
//!   as a number).
//! - **String-prefix bytes survive**: `E'…'` → `E?`, `N'…'` → `N?`. The single-letter
//!   prefix is consumed as a one-character identifier; the literal that follows is
//!   replaced.
//! - **Malformed hex** (`0xZZ`, `0x` at EOF): the hex branch fails because no hex digit
//!   follows; the numeric branch then consumes `0` (→ `?`), and the trailing bytes are
//!   scanned as identifier (→ `?xZZ`, `?x`).
//!
//! # Invariants
//!
//! - **Length monotonicity**: `obfuscate(s).len() <= s.len()` for every `s`.
//! - **Idempotency**: `obfuscate(obfuscate(s)) == obfuscate(s)`. The output character `?`
//!   matches none of the recognised tokens, so a second pass is identity.
//! - **No panic**: the implementation never panics, including on malformed UTF-8 fragments,
//!   unterminated literals, or comments that run to end of input.

/// Replace literal values in `sql` with `?`, preserving structure (whitespace, comments,
/// identifiers, operators, `NULL`). See module docs for the full token grammar and the
/// list of known mild deviations.
pub(crate) fn obfuscate(sql: &str) -> String {
    let bytes = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];

        if b == b'-' && bytes.get(i + 1) == Some(&b'-') {
            i = scan_line_comment(sql, bytes, i, &mut out);
        } else if b == b'/' && bytes.get(i + 1) == Some(&b'*') {
            i = scan_block_comment(sql, bytes, i, &mut out);
        } else if b == b'"' {
            i = scan_quoted_identifier(sql, bytes, i, b'"', &mut out);
        } else if b == b'`' {
            i = scan_quoted_identifier(sql, bytes, i, b'`', &mut out);
        } else if b == b'\'' {
            i = scan_string_literal(bytes, i, &mut out);
        } else if b == b'$' {
            i = scan_dollar(sql, bytes, i, &mut out);
        } else if b == b'0'
            && matches!(bytes.get(i + 1), Some(b'x' | b'X'))
            && bytes.get(i + 2).is_some_and(u8::is_ascii_hexdigit)
        {
            i = scan_hex_literal(bytes, i, &mut out);
        } else if b.is_ascii_digit() {
            i = scan_numeric_literal(bytes, i, &mut out);
        } else if b.is_ascii_alphabetic() || b == b'_' {
            i = scan_identifier(sql, bytes, i, &mut out);
        } else {
            // Fall-through: preserve one Unicode scalar verbatim. `sql[i..]` is on a char
            // boundary because every previous step advanced by full-token spans.
            let ch = sql[i..].chars().next().expect("i < bytes.len()");
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn scan_line_comment(sql: &str, bytes: &[u8], start: usize, out: &mut String) -> usize {
    let mut i = start + 2;
    while i < bytes.len() && bytes[i] != b'\n' {
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

fn scan_string_literal(bytes: &[u8], start: usize, out: &mut String) -> usize {
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
    out.push('?');
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
        // Not a dollar quote: preserve `$` as a single byte.
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
                out.push('?');
                return close_end + 1;
            }
        }
        k += 1;
    }
    // Unterminated: consume to EOF.
    out.push('?');
    bytes.len()
}

fn scan_hex_literal(bytes: &[u8], start: usize, out: &mut String) -> usize {
    let mut i = start + 2;
    while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
        i += 1;
    }
    out.push('?');
    i
}

fn scan_numeric_literal(bytes: &[u8], start: usize, out: &mut String) -> usize {
    let mut i = start + 1;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(bytes.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if bytes.get(j).is_some_and(u8::is_ascii_digit) {
            i = j + 1;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
        }
        // If no digits follow `e`/`E` (or an explicit sign), the exponent prefix is left
        // for the next scan iteration to handle as an identifier.
    }
    out.push('?');
    i
}

fn scan_identifier(sql: &str, bytes: &[u8], start: usize, out: &mut String) -> usize {
    let mut i = start + 1;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
        i += 1;
    }
    let ident = &sql[start..i];
    if ident.eq_ignore_ascii_case("true") || ident.eq_ignore_ascii_case("false") {
        out.push('?');
    } else {
        out.push_str(ident);
    }
    i
}

#[cfg(test)]
mod tests {
    use super::obfuscate;

    fn check(input: &str, expected: &str) {
        assert_eq!(obfuscate(input), expected, "input: {input:?}");
    }

    // ===========================================================================
    // strings
    // ===========================================================================

    #[test]
    fn string_simple() {
        check("'alice'", "?");
    }

    #[test]
    fn string_empty() {
        check("''", "?");
    }

    #[test]
    fn string_doubled_quote_escape() {
        check("'it''s'", "?");
    }

    #[test]
    fn string_backslash_escape() {
        check(r"'it\'s'", "?");
    }

    #[test]
    fn string_escaped_backslash() {
        check(r"'\\'", "?");
    }

    #[test]
    fn string_multiple_in_expression() {
        check("'a' || 'b'", "? || ?");
    }

    #[test]
    fn string_unterminated() {
        check("'unterminated", "?");
    }

    #[test]
    fn string_trailing_backslash_at_eof() {
        // A bare backslash at end of an unterminated string must not read past EOF.
        check("'\\", "?");
    }

    // ===========================================================================
    // numbers (signs are preserved verbatim)
    // ===========================================================================

    #[test]
    fn number_integer() {
        check("42", "?");
    }

    #[test]
    fn number_zero() {
        check("0", "?");
    }

    #[test]
    fn number_decimal() {
        check("3.14", "?");
    }

    #[test]
    fn number_exponent_lowercase() {
        check("1.5e10", "?");
    }

    #[test]
    fn number_exponent_uppercase_signed() {
        check("2E-5", "?");
    }

    #[test]
    fn number_exponent_explicit_plus() {
        check("1e+3", "?");
    }

    #[test]
    fn number_exponent_without_digits_falls_back() {
        // `1e` with no exponent digits: the `e` is not consumed, becomes an identifier.
        check("1e", "?e");
    }

    #[test]
    fn number_hex_lowercase() {
        check("0xFF", "?");
    }

    #[test]
    fn number_hex_uppercase_prefix() {
        check("0XAB", "?");
    }

    #[test]
    fn number_hex_no_digits() {
        check("0xZZ", "?xZZ");
    }

    #[test]
    fn number_hex_prefix_at_eof() {
        check("0x", "?x");
    }

    #[test]
    fn number_unary_minus_preserves_sign() {
        check("WHERE x = -1", "WHERE x = -?");
    }

    #[test]
    fn number_unary_plus_preserves_sign() {
        check("WHERE x = +1", "WHERE x = +?");
    }

    #[test]
    fn number_binary_minus_preserves_sign() {
        check("SELECT a-1 FROM t", "SELECT a-? FROM t");
    }

    #[test]
    fn number_subtract_inside_parens() {
        check("SELECT (1-1)", "SELECT (?-?)");
    }

    #[test]
    fn number_signed_in_between() {
        check("BETWEEN 1 AND -2", "BETWEEN ? AND -?");
    }

    #[test]
    fn number_signed_after_then() {
        check("THEN -2", "THEN -?");
    }

    #[test]
    fn number_identifier_with_trailing_digit() {
        check("col1", "col1");
    }

    #[test]
    fn number_leading_dot() {
        // Documented mild deviation: `.5` is `.` punctuation followed by `5`.
        check(".5", ".?");
    }

    // ===========================================================================
    // booleans and NULL
    // ===========================================================================

    #[test]
    fn boolean_true_lowercase() {
        check("true", "?");
    }

    #[test]
    fn boolean_true_uppercase() {
        check("TRUE", "?");
    }

    #[test]
    fn boolean_true_mixed_case() {
        check("True", "?");
    }

    #[test]
    fn boolean_false_lowercase() {
        check("false", "?");
    }

    #[test]
    fn boolean_false_uppercase() {
        check("FALSE", "?");
    }

    #[test]
    fn null_lowercase() {
        check("null", "null");
    }

    #[test]
    fn null_uppercase() {
        check("NULL", "NULL");
    }

    #[test]
    fn boolean_substring_in_identifier_preserved() {
        check("TRUE_COLUMN", "TRUE_COLUMN");
    }

    #[test]
    fn boolean_truthy_preserved() {
        check("truthy", "truthy");
    }

    #[test]
    fn boolean_falsey_preserved() {
        check("falsey", "falsey");
    }

    // ===========================================================================
    // quoted identifiers
    // ===========================================================================

    #[test]
    fn ansi_quoted_identifier_preserved() {
        check("\"my table\"", "\"my table\"");
    }

    #[test]
    fn backtick_quoted_identifier_preserved() {
        check("`col`", "`col`");
    }

    #[test]
    fn ansi_quoted_with_doubled_escape() {
        check("\"with\"\"quote\"", "\"with\"\"quote\"");
    }

    #[test]
    fn backtick_quoted_with_doubled_escape() {
        check("`with``tick`", "`with``tick`");
    }

    #[test]
    fn ansi_quoted_unterminated_preserved() {
        // Reaches EOF mid-identifier – preserved verbatim, no `?`, no panic.
        check("\"abc", "\"abc");
    }

    #[test]
    fn quoted_identifiers_with_string_literal() {
        check(
            "SELECT \"name\" FROM users WHERE \"name\" = 'alice'",
            "SELECT \"name\" FROM users WHERE \"name\" = ?",
        );
    }

    // ===========================================================================
    // dollar-quoted strings
    // ===========================================================================

    #[test]
    fn dollar_quoted_empty_tag() {
        check("$$body$$", "?");
    }

    #[test]
    fn dollar_quoted_with_tag_containing_inner_quotes() {
        check("$tag$body with 'quotes'$tag$", "?");
    }

    #[test]
    fn dollar_positional_param_one_digit() {
        check("$1", "$1");
    }

    #[test]
    fn dollar_positional_param_multi_digit() {
        check("$42", "$42");
    }

    #[test]
    fn dollar_quoted_unterminated() {
        check("$$ab", "?");
    }

    #[test]
    fn dollar_quoted_unterminated_with_tag() {
        check("$tag$body$ta", "?");
    }

    #[test]
    fn dollar_quoted_adjacent() {
        check("$$a$$$$b$$", "??");
    }

    #[test]
    fn dollar_lone_with_no_opener_preserved() {
        // `$body` is not a dollar quote (no closing `$` of opener); `$` is preserved as
        // punctuation and `body` is an identifier.
        check("$body", "$body");
    }

    // ===========================================================================
    // comments
    // ===========================================================================

    #[test]
    fn line_comment_preserves_content() {
        check("-- secret 42\nSELECT 1", "-- secret 42\nSELECT ?");
    }

    #[test]
    fn line_comment_at_eof() {
        check("-- trailing", "-- trailing");
    }

    #[test]
    fn block_comment_preserves_content() {
        check("/* secret 42 */ SELECT 1", "/* secret 42 */ SELECT ?");
    }

    #[test]
    fn block_comment_unterminated() {
        check("/* unterminated", "/* unterminated");
    }

    #[test]
    fn block_comment_empty() {
        check("/**/SELECT 1", "/**/SELECT ?");
    }

    // ===========================================================================
    // combined / golden cases
    // ===========================================================================

    #[test]
    fn golden_insert_with_inline_literals() {
        check(
            "INSERT INTO users (name, age) VALUES ('alice', 42)",
            "INSERT INTO users (name, age) VALUES (?, ?)",
        );
    }

    #[test]
    fn golden_preserves_whitespace_and_newlines() {
        check(
            "SELECT *\n  FROM users\n  WHERE id = 1",
            "SELECT *\n  FROM users\n  WHERE id = ?",
        );
    }

    #[test]
    fn golden_postgres_e_string_prefix() {
        // Documented mild deviation: the `E` survives as a one-char identifier; the
        // string literal that follows is replaced.
        check(r"E'a\nb'", "E?");
    }

    #[test]
    fn non_ascii_identifier_preserved() {
        // Multi-byte UTF-8 bytes inside an identifier flow through the catch-all branch
        // verbatim, so the round-trip keeps the original spelling.
        check("WHERE café = 'value'", "WHERE café = ?");
    }

    // ===========================================================================
    // property-based tests
    // ===========================================================================

    mod proptests {
        use super::super::obfuscate;
        use proptest::prelude::*;

        // The sentinel must contain bytes that cannot appear inside any preserved token
        // generated by `tokens_with_marked_literals`. Uppercase letters guarantee it
        // cannot land inside the all-lowercase identifier/comment bodies that surround
        // the marked literals.
        const SENTINEL: &str = "XSECRETX";

        fn whitespace() -> impl Strategy<Value = String> {
            "[ \t\n]{0,5}".prop_map(String::from)
        }

        fn ident_lower() -> impl Strategy<Value = String> {
            "[a-z_][a-z0-9_]{0,7}"
                .prop_map(String::from)
                .prop_filter("exclude TRUE/FALSE which the obfuscator replaces", |s| {
                    !s.eq_ignore_ascii_case("true") && !s.eq_ignore_ascii_case("false")
                })
        }

        fn ansi_quoted_ident() -> impl Strategy<Value = String> {
            "[a-z0-9 _]{0,8}".prop_map(|inner| format!("\"{inner}\""))
        }

        fn backtick_quoted_ident() -> impl Strategy<Value = String> {
            "[a-z0-9 _]{0,8}".prop_map(|inner| format!("`{inner}`"))
        }

        fn safe_punct() -> impl Strategy<Value = String> {
            // Excludes characters that combine with neighbours to start tokens that are
            // *not* preserved: `-` (could form `--`), `/` (could form `/*`), `'` `"` `` `
            // `$` (literal/identifier delimiters), `.` (could combine with a digit to form
            // `.5`), digits (numeric literals).
            prop::sample::select(vec![",", ";", "=", "<", ">", "+", "*", "(", ")"])
                .prop_map(String::from)
        }

        fn integer() -> impl Strategy<Value = String> {
            "[0-9]{1,5}".prop_map(String::from)
        }

        fn decimal() -> impl Strategy<Value = String> {
            "[0-9]{1,3}\\.[0-9]{1,3}".prop_map(String::from)
        }

        fn hex_literal() -> impl Strategy<Value = String> {
            "0[xX][0-9a-fA-F]{1,4}".prop_map(String::from)
        }

        fn string_literal_plain() -> impl Strategy<Value = String> {
            // Body excludes `\` and `'` so we don't have to reason about escape chains in
            // the generator itself.
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

        fn boolean_kw() -> impl Strategy<Value = String> {
            prop::sample::select(vec!["TRUE", "FALSE", "true", "false", "True", "False"])
                .prop_map(String::from)
        }

        /// Any-token generator: produces every token kind, including all literal forms.
        /// Used for invariants that must hold over the full grammar (no-panic,
        /// idempotency, length monotonicity).
        fn token_any() -> impl Strategy<Value = String> {
            prop_oneof![
                ident_lower(),
                whitespace(),
                ansi_quoted_ident(),
                backtick_quoted_ident(),
                safe_punct(),
                integer(),
                decimal(),
                hex_literal(),
                string_literal_plain(),
                dollar_quoted_plain(),
                line_comment(),
                block_comment(),
                boolean_kw(),
            ]
        }

        fn fragment_any() -> impl Strategy<Value = String> {
            prop::collection::vec(token_any(), 0..16).prop_map(|tokens| tokens.concat())
        }

        /// Preservable-only generator: every produced token must round-trip through
        /// `obfuscate` unchanged. Excludes literals (string, dollar, numeric, hex,
        /// boolean) and the byte sequences that begin comments (which would otherwise be
        /// preserved but only when matched as a comment, not when sliced across tokens).
        fn token_preservable() -> impl Strategy<Value = String> {
            prop_oneof![
                ident_lower(),
                whitespace(),
                ansi_quoted_ident(),
                backtick_quoted_ident(),
                safe_punct(),
                Just("NULL".to_string()),
                Just("null".to_string()),
            ]
        }

        fn fragment_preservable() -> impl Strategy<Value = String> {
            prop::collection::vec(token_preservable(), 0..16).prop_map(|tokens| tokens.concat())
        }

        /// Marked literals: string and dollar bodies always embed the sentinel. Used by
        /// the no-leak property to detect any literal kind that escapes obfuscation.
        fn marked_string() -> impl Strategy<Value = String> {
            Just(format!("'{SENTINEL}'"))
        }

        fn marked_dollar() -> impl Strategy<Value = String> {
            "[a-z_]{0,3}".prop_map(|tag| format!("${tag}${SENTINEL}${tag}$"))
        }

        fn token_marked() -> impl Strategy<Value = String> {
            prop_oneof![
                ident_lower(),
                whitespace(),
                ansi_quoted_ident(),
                backtick_quoted_ident(),
                safe_punct(),
                marked_string(),
                marked_dollar(),
            ]
        }

        fn fragment_marked() -> impl Strategy<Value = String> {
            prop::collection::vec(token_marked(), 0..12).prop_map(|tokens| tokens.concat())
        }

        // Digit-free preservables: identical to `token_preservable` but with bodies
        // restricted to characters that cannot leak ASCII digits if echoed verbatim.
        // Used by `no_digit_leak` to detect numeric/hex obfuscation regressions.

        fn ident_lower_no_digits() -> impl Strategy<Value = String> {
            "[a-z_]{1,8}"
                .prop_map(String::from)
                .prop_filter("exclude TRUE/FALSE which the obfuscator replaces", |s| {
                    !s.eq_ignore_ascii_case("true") && !s.eq_ignore_ascii_case("false")
                })
        }

        fn ansi_quoted_no_digits() -> impl Strategy<Value = String> {
            "[a-z _]{0,8}".prop_map(|inner| format!("\"{inner}\""))
        }

        fn backtick_quoted_no_digits() -> impl Strategy<Value = String> {
            "[a-z _]{0,8}".prop_map(|inner| format!("`{inner}`"))
        }

        fn token_digit_free() -> impl Strategy<Value = String> {
            prop_oneof![
                // Preservables that must echo verbatim and cannot leak digits.
                ident_lower_no_digits(),
                whitespace(),
                ansi_quoted_no_digits(),
                backtick_quoted_no_digits(),
                safe_punct(),
                Just("NULL".to_string()),
                // Replaceable literal kinds whose payload is digit-bearing. If the
                // obfuscator misses any of them, digits leak into the output.
                integer(),
                decimal(),
                hex_literal(),
                // String/dollar literals are digit-free in their generated bodies, so
                // their inclusion exercises mixed-token sequences without contributing
                // digits via preservation.
                marked_string(),
                marked_dollar(),
            ]
        }

        fn fragment_digit_free() -> impl Strategy<Value = String> {
            // Mandatory whitespace between tokens prevents adjacent literals from fusing
            // into hybrid tokens (e.g. `1` + `0xFF` → `10xFF`, where the obfuscator's
            // numeric branch would consume the `0` and leave `xFF` as an identifier).
            prop::collection::vec(token_digit_free(), 0..16).prop_map(|tokens| tokens.join(" "))
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(256))]

            /// `obfuscate` must never panic on any input, including malformed UTF-8 and
            /// arbitrary byte noise.
            #[test]
            fn no_panic_on_random_bytes(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
                let s = String::from_utf8_lossy(&bytes).into_owned();
                let _ = obfuscate(&s);
            }

            /// Same invariant against syntactically plausible SQL fragments – penetrates
            /// the state machine where random bytes might not.
            #[test]
            fn no_panic_on_structured_fragments(s in fragment_any()) {
                let _ = obfuscate(&s);
            }

            /// The placeholder `?` matches none of the recognised tokens, so a second
            /// pass must be an identity.
            #[test]
            fn idempotent(s in fragment_any()) {
                let once = obfuscate(&s);
                let twice = obfuscate(&once);
                prop_assert_eq!(once, twice);
            }

            /// Each replacement is a single byte for an input span of ≥ 1 byte, so the
            /// output cannot grow.
            #[test]
            fn length_monotonic(s in fragment_any()) {
                let out = obfuscate(&s);
                prop_assert!(out.len() <= s.len());
            }

            /// Inputs built only from preserved-token kinds must round-trip identically.
            #[test]
            fn preservable_round_trip(s in fragment_preservable()) {
                prop_assert_eq!(obfuscate(&s), s);
            }

            /// Every literal in the input embeds the sentinel; if any literal kind is
            /// not detected, the sentinel leaks into the output. The generator excludes
            /// the sentinel from preserved tokens, so any leak is a real failure.
            #[test]
            fn no_leak_through_literals(s in fragment_marked()) {
                let out = obfuscate(&s);
                prop_assert!(
                    !out.contains(SENTINEL),
                    "sentinel leaked: input={s:?} output={out:?}"
                );
            }

            /// Digit-leak property: the input mixes digit-bearing literals (integer,
            /// decimal, hex) with preservables whose bodies contain no digits. If any
            /// numeric literal kind is not obfuscated, digits leak into the output.
            /// Complements `no_leak_through_literals`, which can only catch string/
            /// dollar regressions because the alphabetic sentinel `XSECRETX` cannot
            /// embed in numeric digit runs.
            #[test]
            fn no_digit_leak(s in fragment_digit_free()) {
                let out = obfuscate(&s);
                prop_assert!(
                    !out.chars().any(|c| c.is_ascii_digit()),
                    "digit leaked: input={s:?} output={out:?}"
                );
            }
        }
    }
}
