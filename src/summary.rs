//! Best-effort low-cardinality SQL query summaries.
//!
//! The OpenTelemetry database semantic conventions recommend using
//! `db.query.summary` as the span name when query-level instrumentation can derive one.
//! This module intentionally extracts only the outer operation and its primary target.
//! Predicates, selected columns, literal values, and bind parameters never participate in
//! the summary.

const MAX_SUMMARY_BYTES: usize = 255;
const MAX_CTE_RESOLUTION_DEPTH: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Word(String),
    QuotedIdentifier(String),
    Dot,
    LeftParen,
    RightParen,
    Comma,
    Semicolon,
}

#[derive(Clone, Debug)]
struct Cte {
    key: String,
    body: Vec<Token>,
}

#[derive(Clone, Debug)]
struct Target {
    display: String,
    key: Option<String>,
}

/// Return a short summary such as `SELECT users` or `INSERT orders`.
pub(crate) fn summarize(sql: &str) -> Option<String> {
    // Run the same literal scrubber used by QueryTextMode::Obfuscated before parsing.
    // Query summaries therefore cannot accidentally incorporate a literal even if the
    // lightweight tokenizer below encounters malformed SQL.
    let sanitized = crate::obfuscate::obfuscate(sql);
    let tokens = tokenize(&sanitized);
    let (operation, target) = summarize_tokens(&tokens, &[], &mut Vec::new(), 0)?;
    let Some(target) = target else {
        return Some(operation);
    };
    let candidate = format!("{operation} {}", target.display);
    if candidate.len() <= MAX_SUMMARY_BYTES {
        Some(candidate)
    } else {
        // Never truncate inside an identifier. Falling back to the operation preserves a
        // useful, bounded grouping key.
        Some(operation)
    }
}

fn summarize_tokens(
    tokens: &[Token],
    inherited_cte_scopes: &[&[Cte]],
    visited_ctes: &mut Vec<String>,
    resolution_depth: usize,
) -> Option<(String, Option<Target>)> {
    let (local_ctes, statement_start) = parse_ctes(tokens);
    let (operation, operation_index) = find_operation(tokens, statement_start)?;
    let target = find_target(tokens, operation_index, &operation).map(|target| {
        // A write target names the physical object even when a CTE shadows its name.
        if !operation.eq_ignore_ascii_case("SELECT") {
            return target;
        }
        resolve_cte_target(
            target,
            &local_ctes,
            inherited_cte_scopes,
            visited_ctes,
            resolution_depth,
        )
    });
    Some((operation, target))
}

fn resolve_cte_target(
    target: Target,
    local_ctes: &[Cte],
    inherited_cte_scopes: &[&[Cte]],
    visited_ctes: &mut Vec<String>,
    resolution_depth: usize,
) -> Target {
    if resolution_depth >= MAX_CTE_RESOLUTION_DEPTH {
        return target;
    }
    let Some(ref key) = target.key else {
        return target;
    };
    if visited_ctes.iter().any(|visited| visited == key) {
        return target;
    }
    let cte = local_ctes.iter().find(|cte| cte.key == *key).or_else(|| {
        inherited_cte_scopes
            .iter()
            .find_map(|scope| scope.iter().find(|cte| cte.key == *key))
    });
    let Some(cte) = cte else {
        return target;
    };

    visited_ctes.push(key.clone());
    let mut scopes = Vec::with_capacity(inherited_cte_scopes.len() + 1);
    scopes.push(local_ctes);
    scopes.extend_from_slice(inherited_cte_scopes);
    let resolved = summarize_tokens(&cte.body, &scopes, visited_ctes, resolution_depth + 1)
        .and_then(|(_, nested_target)| nested_target)
        .unwrap_or(target);
    visited_ctes.pop();
    resolved
}

fn parse_ctes(tokens: &[Token]) -> (Vec<Cte>, usize) {
    if !token_is_keyword(tokens.first(), "WITH") {
        return (Vec::new(), 0);
    }

    let mut ctes = Vec::new();
    let mut index = 1;
    if token_is_keyword(tokens.get(index), "RECURSIVE") {
        index += 1;
    }

    loop {
        let Some(alias) = tokens.get(index).and_then(identifier_key) else {
            return (ctes, index);
        };
        index += 1;

        // Optional CTE column list: name(col_a, col_b) AS (...).
        if matches!(tokens.get(index), Some(Token::LeftParen)) {
            let Some(close) = matching_right_paren(tokens, index) else {
                return (ctes, index);
            };
            index = close + 1;
        }
        if !token_is_keyword(tokens.get(index), "AS") {
            return (ctes, index);
        }
        index += 1;
        if token_is_keyword(tokens.get(index), "NOT") {
            index += 1;
        }
        if token_is_keyword(tokens.get(index), "MATERIALIZED") {
            index += 1;
        }
        if !matches!(tokens.get(index), Some(Token::LeftParen)) {
            return (ctes, index);
        }
        let Some(close) = matching_right_paren(tokens, index) else {
            return (ctes, index);
        };
        ctes.push(Cte {
            key: alias,
            body: tokens[index + 1..close].to_vec(),
        });
        index = close + 1;
        if matches!(tokens.get(index), Some(Token::Comma)) {
            index += 1;
            continue;
        }
        return (ctes, index);
    }
}

fn matching_right_paren(tokens: &[Token], open: usize) -> Option<usize> {
    let mut depth = 0_u32;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token {
            Token::LeftParen => depth = depth.saturating_add(1),
            Token::RightParen if depth == 1 => return Some(index),
            Token::RightParen => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

fn find_operation(tokens: &[Token], start: usize) -> Option<(String, usize)> {
    let mut depth = 0_u32;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        match token {
            Token::LeftParen => depth = depth.saturating_add(1),
            Token::RightParen => depth = depth.saturating_sub(1),
            Token::Semicolon if depth == 0 => return None,
            Token::Word(word) if depth == 0 && is_operation(word) => {
                return Some((word.clone(), index));
            }
            _ => {}
        }
    }
    None
}

fn find_target(tokens: &[Token], operation_index: usize, operation: &str) -> Option<Target> {
    if operation.eq_ignore_ascii_case("SELECT") {
        let from = find_keyword_at_depth(tokens, operation_index + 1, "FROM")?;
        return parse_target(tokens, from + 1);
    }
    if operation.eq_ignore_ascii_case("INSERT") || operation.eq_ignore_ascii_case("MERGE") {
        let into = find_keyword_at_depth(tokens, operation_index + 1, "INTO")?;
        return parse_target(tokens, into + 1);
    }
    if operation.eq_ignore_ascii_case("DELETE") {
        let from = find_keyword_at_depth(tokens, operation_index + 1, "FROM")?;
        return parse_target(tokens, from + 1);
    }
    if operation.eq_ignore_ascii_case("UPDATE")
        || operation.eq_ignore_ascii_case("COPY")
        || operation.eq_ignore_ascii_case("CALL")
        || operation.eq_ignore_ascii_case("EXECUTE")
    {
        return parse_target(tokens, operation_index + 1);
    }
    if operation.eq_ignore_ascii_case("TRUNCATE") {
        let table = find_keyword_at_depth(tokens, operation_index + 1, "TABLE");
        return parse_target(tokens, table.map_or(operation_index + 1, |index| index + 1));
    }
    if operation.eq_ignore_ascii_case("CREATE")
        || operation.eq_ignore_ascii_case("ALTER")
        || operation.eq_ignore_ascii_case("DROP")
    {
        let object_kind = find_any_keyword_at_depth(
            tokens,
            operation_index + 1,
            &["TABLE", "VIEW", "INDEX", "SCHEMA", "DATABASE"],
        )?;
        return parse_target(tokens, object_kind + 1);
    }
    None
}

fn find_keyword_at_depth(tokens: &[Token], start: usize, keyword: &str) -> Option<usize> {
    find_any_keyword_at_depth(tokens, start, &[keyword])
}

fn find_any_keyword_at_depth(tokens: &[Token], start: usize, keywords: &[&str]) -> Option<usize> {
    let mut depth = 0_u32;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        match token {
            Token::LeftParen => depth = depth.saturating_add(1),
            Token::RightParen => depth = depth.saturating_sub(1),
            Token::Semicolon if depth == 0 => return None,
            Token::Word(word)
                if depth == 0
                    && keywords
                        .iter()
                        .any(|keyword| word.eq_ignore_ascii_case(keyword)) =>
            {
                return Some(index);
            }
            _ => {}
        }
    }
    None
}

fn parse_target(tokens: &[Token], mut index: usize) -> Option<Target> {
    while let Some(Token::Word(word)) = tokens.get(index) {
        if !is_target_modifier(word) {
            break;
        }
        index += 1;
    }

    let first = tokens.get(index)?;
    let first_display = identifier_display(first)?;
    if matches!(first, Token::Word(word) if is_clause_keyword(word)) {
        return None;
    }
    let mut display = first_display.to_owned();
    let mut key = identifier_key(first);
    index += 1;

    while matches!(tokens.get(index), Some(Token::Dot)) {
        let next = tokens.get(index + 1)?;
        let next_display = identifier_display(next)?;
        if matches!(next, Token::Word(word) if is_clause_keyword(word)) {
            break;
        }
        display.push('.');
        display.push_str(next_display);
        key = None;
        index += 2;
    }

    Some(Target { display, key })
}

fn token_is_keyword(token: Option<&Token>, keyword: &str) -> bool {
    matches!(token, Some(Token::Word(word)) if word.eq_ignore_ascii_case(keyword))
}

fn identifier_display(token: &Token) -> Option<&str> {
    match token {
        Token::Word(word) | Token::QuotedIdentifier(word) => Some(word),
        _ => None,
    }
}

fn identifier_key(token: &Token) -> Option<String> {
    match token {
        Token::Word(word) if !is_clause_keyword(word) => Some(word.to_ascii_lowercase()),
        Token::QuotedIdentifier(identifier) => Some(identifier.clone()),
        _ => None,
    }
}

fn is_operation(word: &str) -> bool {
    [
        "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "COPY", "CALL", "EXECUTE", "CREATE",
        "ALTER", "DROP", "TRUNCATE", "BEGIN", "COMMIT", "ROLLBACK", "EXPLAIN",
    ]
    .iter()
    .any(|operation| word.eq_ignore_ascii_case(operation))
}

fn is_target_modifier(word: &str) -> bool {
    [
        "ONLY",
        "LATERAL",
        "IF",
        "NOT",
        "EXISTS",
        "OR",
        "REPLACE",
        "IGNORE",
        "LOW_PRIORITY",
        "HIGH_PRIORITY",
        "DELAYED",
        "TEMP",
        "TEMPORARY",
        "UNLOGGED",
        "CONCURRENTLY",
    ]
    .iter()
    .any(|modifier| word.eq_ignore_ascii_case(modifier))
}

fn is_clause_keyword(word: &str) -> bool {
    [
        "SELECT",
        "FROM",
        "WHERE",
        "GROUP",
        "ORDER",
        "HAVING",
        "LIMIT",
        "OFFSET",
        "FETCH",
        "JOIN",
        "LEFT",
        "RIGHT",
        "FULL",
        "INNER",
        "OUTER",
        "CROSS",
        "ON",
        "USING",
        "UNION",
        "INTERSECT",
        "EXCEPT",
        "RETURNING",
        "VALUES",
        "SET",
        "AS",
        "INTO",
        "UPDATE",
        "DELETE",
        "INSERT",
        "MERGE",
    ]
    .iter()
    .any(|keyword| word.eq_ignore_ascii_case(keyword))
}

fn tokenize(sql: &str) -> Vec<Token> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => index += 1,
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index = skip_block_comment(bytes, index);
            }
            b'"' | b'`' => {
                let quote = bytes[index];
                let end = scan_doubled_quote(bytes, index, quote);
                tokens.push(Token::QuotedIdentifier(sql[index..end].to_owned()));
                index = end;
            }
            b'[' => {
                let end = scan_bracket_identifier(bytes, index);
                tokens.push(Token::QuotedIdentifier(sql[index..end].to_owned()));
                index = end;
            }
            b'(' => {
                tokens.push(Token::LeftParen);
                index += 1;
            }
            b')' => {
                tokens.push(Token::RightParen);
                index += 1;
            }
            b',' => {
                tokens.push(Token::Comma);
                index += 1;
            }
            b'.' => {
                tokens.push(Token::Dot);
                index += 1;
            }
            b';' => {
                tokens.push(Token::Semicolon);
                index += 1;
            }
            byte if is_word_start(byte) => {
                let start = index;
                index += 1;
                while index < bytes.len() && is_word_continue(bytes[index]) {
                    index += 1;
                }
                tokens.push(Token::Word(sql[start..index].to_owned()));
            }
            _ => index += 1,
        }
    }

    tokens
}

fn skip_block_comment(bytes: &[u8], mut index: usize) -> usize {
    let mut depth = 0_u32;
    while index < bytes.len() {
        if bytes.get(index..index + 2) == Some(b"/*") {
            depth = depth.saturating_add(1);
            index += 2;
        } else if bytes.get(index..index + 2) == Some(b"*/") {
            index += 2;
            depth = depth.saturating_sub(1);
            if depth == 0 {
                break;
            }
        } else {
            index += 1;
        }
    }
    index
}

fn scan_doubled_quote(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == quote {
            if bytes.get(index + 1) == Some(&quote) {
                index += 2;
                continue;
            }
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

fn scan_bracket_identifier(bytes: &[u8], start: usize) -> usize {
    let mut index = start + 1;
    while index < bytes.len() {
        if bytes[index] == b']' {
            if bytes.get(index + 1) == Some(&b']') {
                index += 2;
                continue;
            }
            return index + 1;
        }
        index += 1;
    }
    bytes.len()
}

fn is_word_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_' || byte >= 0x80
}

fn is_word_continue(byte: u8) -> bool {
    is_word_start(byte) || byte.is_ascii_digit() || byte == b'$'
}

#[cfg(test)]
mod tests {
    use super::summarize;

    #[test]
    fn summarizes_simple_statements() {
        let cases = [
            ("SELECT * FROM users WHERE id = $1", "SELECT users"),
            ("INSERT INTO orders(id) VALUES ($1)", "INSERT orders"),
            ("UPDATE orders SET status = $1", "UPDATE orders"),
            ("DELETE FROM orders WHERE id = $1", "DELETE orders"),
            (
                "MERGE INTO inventory USING incoming ON true",
                "MERGE inventory",
            ),
            ("CALL refresh_inventory($1)", "CALL refresh_inventory"),
            ("TRUNCATE TABLE audit_log", "TRUNCATE audit_log"),
            ("CREATE TABLE users(id bigint)", "CREATE users"),
        ];
        for (sql, expected) in cases {
            assert_eq!(summarize(sql).as_deref(), Some(expected), "sql: {sql}");
        }
    }

    #[test]
    fn resolves_outer_cte_to_primary_physical_target() {
        let sql = r"
            WITH source_rows AS (
                SELECT event.id
                FROM events event
                JOIN leagues league ON league.id = event.league_id
            )
            SELECT COUNT(*) FROM source_rows
        ";
        assert_eq!(summarize(sql).as_deref(), Some("SELECT events"));
    }

    #[test]
    fn resolves_chained_ctes() {
        let sql = r"
            WITH baseline AS (
                SELECT id FROM events
            ),
            source_rows AS (
                SELECT id FROM baseline
            ),
            evaluated AS (
                SELECT id FROM source_rows
            )
            SELECT COUNT(*) FROM evaluated
        ";
        assert_eq!(summarize(sql).as_deref(), Some("SELECT events"));
    }

    #[test]
    fn insert_with_ctes_keeps_outer_write_target() {
        let sql = r"
            WITH baseline AS (SELECT id FROM events)
            INSERT INTO event_reports(event_id)
            SELECT id FROM baseline
        ";
        assert_eq!(summarize(sql).as_deref(), Some("INSERT event_reports"));
    }

    #[test]
    fn write_targets_are_not_resolved_through_cte_aliases() {
        for statement in [
            "INSERT INTO reports SELECT * FROM reports",
            "UPDATE reports SET id = 1",
            "DELETE FROM reports",
        ] {
            let sql = format!("WITH reports AS (SELECT id FROM source) {statement}");
            let operation = statement.split_whitespace().next().unwrap();
            assert_eq!(summarize(&sql), Some(format!("{operation} reports")));
        }
    }

    #[test]
    fn preserves_qualified_and_quoted_targets() {
        assert_eq!(
            summarize(r#"SELECT * FROM "reporting"."daily orders" WHERE id = 42"#).as_deref(),
            Some(r#"SELECT "reporting"."daily orders""#)
        );
        assert_eq!(
            summarize("SELECT * FROM `daily orders`").as_deref(),
            Some("SELECT `daily orders`")
        );
    }

    #[test]
    fn ignores_comments_and_literal_contents() {
        let sql = r"
            /* SELECT secrets FROM credentials */
            SELECT '-- FROM hidden' FROM public.events
            WHERE token = 'sensitive'
        ";
        assert_eq!(summarize(sql).as_deref(), Some("SELECT public.events"));
    }

    #[test]
    fn select_without_a_physical_target_uses_operation_only() {
        assert_eq!(summarize("SELECT 1").as_deref(), Some("SELECT"));
        assert_eq!(
            summarize("SELECT * FROM (SELECT * FROM users) nested").as_deref(),
            Some("SELECT")
        );
    }

    #[test]
    fn preserves_operation_casing() {
        assert_eq!(
            summarize("select * from users").as_deref(),
            Some("select users")
        );
    }

    #[test]
    fn malformed_and_non_query_input_fails_closed() {
        assert_eq!(summarize(""), None);
        assert_eq!(summarize("this is not sql"), None);
        assert_eq!(summarize("WITH broken AS ("), None);
    }

    #[test]
    fn never_truncates_inside_a_target() {
        let target = format!("table_{}", "x".repeat(300));
        let sql = format!("SELECT * FROM {target}");
        assert_eq!(summarize(&sql).as_deref(), Some("SELECT"));
    }

    #[test]
    fn recursive_cte_does_not_recurse_forever() {
        let sql = "WITH RECURSIVE loop AS (SELECT * FROM loop) SELECT * FROM loop";
        assert_eq!(summarize(sql).as_deref(), Some("SELECT loop"));
    }
}
