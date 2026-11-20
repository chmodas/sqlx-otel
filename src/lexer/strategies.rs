//! Dialect-aware `proptest` generators shared by the lexer, obfuscator, compactor and executor test
//! suites.
//!
//! Every generator emits SQL that is lexically valid in its dialect, paired with the exact output
//! the consumer under test must produce. Bodies of literals, comments and quoted identifiers are
//! drawn from a hostile palette – quotes of every kind, backslashes, `$`, `#`, brackets, comment
//! markers, non-ASCII – so a lexer that mis-reads any construct loses synchronisation and the
//! mismatch surfaces as a wrong output or a leaked [`SENTINEL`].
//!
//! Keeping the generators in one place means the three suites cannot drift apart: a token shape
//! changes once, here.

use proptest::prelude::*;

use super::Dialect;

/// Marker embedded in every generated literal. It is uppercase, and no generated non-literal region
/// contains an uppercase `X`, so finding it in obfuscated output means a literal leaked.
pub(crate) const SENTINEL: &str = "XSECRETX";

/// Any of the three dialects.
pub(crate) fn dialect() -> impl Strategy<Value = Dialect> {
    prop_oneof![
        Just(Dialect::Postgres),
        Just(Dialect::MySql),
        Just(Dialect::Sqlite)
    ]
}

/// SQL-shaped noise: delimiter-heavy ASCII mixed with multi-byte characters, so most inputs start,
/// end or straddle quotes, comments and dollar quotes rather than being plain words. Used for
/// invariants that must hold on malformed input too.
pub(crate) fn sql_noise() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop::sample::select(vec![
            "'", "\"", "`", "[", "]", "\\", "-", "--", "#", "/*", "*/", "/*!", "/*M!", "$", "$$",
            "$t$", "@", ":", "::", "(", ")", "E", "e", "a", "1", "0x", ".", "+", " ", "\n", "\r",
            "\t", "\x0b", "\x0c", "\x7f", "?", "é", "日",
        ]),
        0..40,
    )
    .prop_map(|parts| parts.concat())
}

/// The hostile palette for literal, comment and identifier bodies. Contains no line terminators;
/// constructs that may hold them add them explicitly.
const PALETTE: &[&str] = &[
    "a", "z", "0", "9", " ", "\t", "_", "'", "\"", "`", "\\", "$", "[", "]", "#", "-", "/", "*",
    "(", ")", "!", "é", "日",
];

/// A body built from `PALETTE` minus `exclude`, plus the multi-character `extra` pieces (escape
/// sequences that are legal inside the construct).
fn body(
    exclude: &'static [&'static str],
    extra: &'static [&'static str],
) -> impl Strategy<Value = String> {
    let pieces: Vec<&'static str> = PALETTE
        .iter()
        .copied()
        .filter(|p| !exclude.contains(p))
        .chain(extra.iter().copied())
        .collect();
    prop::collection::vec(prop::sample::select(pieces), 0..10).prop_map(|v| v.concat())
}

/// A body with [`SENTINEL`] somewhere inside it.
fn marked_body(
    exclude: &'static [&'static str],
    extra: &'static [&'static str],
) -> impl Strategy<Value = String> {
    (body(exclude, extra), body(exclude, extra)).prop_map(|(a, b)| format!("{a}{SENTINEL}{b}"))
}

/// A token whose obfuscated form is itself.
fn same(s: String) -> (String, String) {
    (s.clone(), s)
}

/// A token whose obfuscated form is `?`.
fn redacted(s: String) -> (String, String) {
    (s, "?".to_owned())
}

/// A plain identifier that the obfuscator keeps (never `true`/`false`).
pub(crate) fn word() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z_][a-z0-9_]{0,6}".prop_map(String::from),
        "[éß日][a-z]{0,3}".prop_map(String::from),
    ]
    .prop_filter("true/false are literals", |s| {
        !s.eq_ignore_ascii_case("true") && !s.eq_ignore_ascii_case("false")
    })
}

/// A numeric literal in any of the forms the lexer folds into one token.
pub(crate) fn number() -> impl Strategy<Value = String> {
    prop_oneof![
        "[0-9][0-9_]{0,5}[0-9]?",
        "0[xX][0-9a-fA-F]{1,4}",
        "0[bB][01]{1,6}",
        "0[oO][0-7]{1,4}",
        "[0-9]{1,3}\\.[0-9]{0,3}",
        "[0-9]{1,3}[eE][+-]?[0-9]{1,2}",
        "[0-9]{1,2}[a-z]{1,3}",
    ]
    .prop_map(String::from)
}

/// Separators placed between generated tokens: whitespace and punctuation that cannot fuse with any
/// neighbouring token into a different token.
pub(crate) fn separator() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![" ", "\n", "\t", "\r\n", ", ", " (", ") ", " = "])
}

/// A `'…'` string carrying the sentinel, valid in `dialect`.
fn single_quoted_marked(dialect: Dialect) -> BoxedStrategy<String> {
    match dialect {
        // Backslash is an ordinary character, even as the last byte before the closing quote.
        Dialect::Postgres | Dialect::Sqlite => marked_body(&["'"], &["''"]).boxed(),
        Dialect::MySql => marked_body(&["'", "\\"], &["''", "\\\\", "\\'", "\\\""]).boxed(),
    }
    .prop_map(|b| format!("'{b}'"))
    .boxed()
}

/// The body of a `PostgreSQL` E-string, where backslash escapes.
fn e_string_body(marked: bool) -> BoxedStrategy<String> {
    const EXCLUDE: &[&str] = &["'", "\\"];
    const EXTRA: &[&str] = &["''", "\\\\", "\\'"];
    if marked {
        marked_body(EXCLUDE, EXTRA).boxed()
    } else {
        body(EXCLUDE, EXTRA).boxed()
    }
}

/// A line comment, including its `\n` terminator, valid in `dialect`.
fn line_comment(dialect: Dialect) -> BoxedStrategy<String> {
    match dialect {
        Dialect::Postgres => body(&[], &[]).prop_map(|b| format!("--{b}\n")).boxed(),
        Dialect::Sqlite => body(&[], &["\r"]).prop_map(|b| format!("--{b}\n")).boxed(),
        Dialect::MySql => prop_oneof![
            body(&[], &["\r"]).prop_map(|b| format!("-- {b}\n")),
            body(&[], &["\r"]).prop_map(|b| format!("--\x7f{b}\n")),
            body(&[], &["\r"]).prop_map(|b| format!("#{b}\n")),
        ]
        .boxed(),
    }
}

/// A comment body that may contain `*` and `/` but never a `/*` or `*/` of its own.
fn comment_body() -> impl Strategy<Value = String> {
    body(&[], &["\n", "*", "/", "**", "//"])
        .prop_filter("no comment markers inside the body", |b| {
            !b.contains("/*") && !b.contains("*/")
        })
}

/// A terminated block comment valid in `dialect` (nested on `PostgreSQL`). Bodies hold `*` and `/`
/// but no marker of their own, and each piece is checked against the delimiter beside it: a body
/// ending in `*` before an opener would close early (`*/*`), and one ending in `/` before a closer
/// would open a nested comment (`/*/`). Every comment starts `/* `, so a `MySQL` comment is never
/// an executable `/*!`.
fn block_comment(dialect: Dialect) -> BoxedStrategy<String> {
    if dialect == Dialect::Postgres {
        (comment_body(), comment_body(), comment_body())
            .prop_filter("delimiters must not fuse with body edges", |(a, b, c)| {
                !a.ends_with('*') && !b.ends_with('/') && !c.ends_with('/')
            })
            .prop_map(|(a, b, c)| format!("/* {a}/* {b}*/{c}*/"))
            .boxed()
    } else {
        comment_body().prop_map(|b| format!("/* {b}*/")).boxed()
    }
}

/// A terminated quoted identifier valid in `dialect`, with a hostile body.
fn quoted_ident(dialect: Dialect) -> BoxedStrategy<String> {
    let double = body(&["\""], &["\"\""]).prop_map(|b| format!("\"{b}\""));
    let backtick = body(&["`"], &["``"]).prop_map(|b| format!("`{b}`"));
    let bracket = body(&["]"], &[]).prop_map(|b| format!("[{b}]"));
    match dialect {
        Dialect::Postgres => double.boxed(),
        Dialect::MySql => backtick.boxed(),
        Dialect::Sqlite => prop_oneof![double, backtick, bracket].boxed(),
    }
}

/// Split the digits after `/*!` (or `/*M!` when `mariadb`) into the version the server takes and
/// the numeric literal left over: five digits when five are present, six after `/*M!` when six are,
/// and otherwise none.
fn split_exec_version(digits: &str, mariadb: bool) -> (&str, &str) {
    let version = match digits.len() {
        6.. if mariadb => 6,
        5.. => 5,
        _ => 0,
    };
    digits.split_at(version)
}

/// One `(input, expected obfuscation)` pair valid in `dialect`. Both strings are emitted without
/// surrounding whitespace; [`obfuscation_case`] joins them with [`separator`]s.
pub(crate) fn obfuscation_token(dialect: Dialect) -> BoxedStrategy<(String, String)> {
    let common = prop_oneof![
        4 => word().prop_map(same),
        2 => number().prop_map(redacted),
        1 => prop::sample::select(vec!["TRUE", "false", "True"]).prop_map(|s| redacted(s.into())),
        1 => Just(same("NULL".into())),
        2 => prop::sample::select(vec![",", ";", "=", "<", ">", "+", "*", "(", ")", "?", "%"])
            .prop_map(|s| same(s.into())),
        4 => single_quoted_marked(dialect).prop_map(redacted),
        2 => line_comment(dialect).prop_map(same),
        2 => block_comment(dialect).prop_map(same),
        2 => quoted_ident(dialect).prop_map(same),
        // Any digit-led run is one literal, however many letters follow.
        1 => ("[0-9]{1,3}", "[a-z]{1,3}").prop_map(|(d, l)| redacted(format!("{d}{l}"))),
    ];
    let specific = match dialect {
        Dialect::Postgres => prop_oneof![
            // E-strings. The trailing `,` stops a newline separator from turning the next
            // string into a continuation; continuations have their own case below.
            e_string_body(true).prop_map(|b| (format!("E'{b}',"), "E?,".to_owned())),
            ("[a-z_]{0,3}", body(&["$"], &[]))
                .prop_map(|(t, b)| redacted(format!("${t}${b}{SENTINEL}${t}$"))),
            "\\$[1-9][0-9]{0,2}".prop_map(same),
            // Operator characters on PostgreSQL that start constructs elsewhere.
            prop::sample::select(vec!["#", "`", "[", "]"]).prop_map(|s| same(s.into())),
            // `$` continues an identifier, so `w$t$` is a word and not a dollar-quote opener.
            (word(), "[a-z]{1,3}").prop_map(|(w, t)| (
                format!("{w}${t}$, '{SENTINEL}${t}$'"),
                format!("{w}${t}$, ?"),
            )),
            // A non-ASCII byte starts a word, so the `E` after it is not an E-string prefix.
            (
                prop::sample::select(vec!["é", "日", "ß"]),
                marked_body(&["'"], &["''"])
            )
                .prop_map(|(n, b)| (format!("{n}E'{b}'"), format!("{n}E?"))),
            // E-string continuation across a newline-bearing gap.
            (
                e_string_body(false),
                prop::sample::select(vec![
                    "\n",
                    " \n ",
                    "\r\n",
                    "\r",
                    " -- c'\n",
                    "\n-- c\n\t",
                    "\x0b\n",
                    "\x0c\n\x0b",
                ]),
                e_string_body(true),
            )
                .prop_map(|(a, gap, b)| (format!("E'{a}'{gap}'{b}',"), "E?,".to_owned())),
        ]
        .boxed(),
        Dialect::MySql => prop_oneof![
            marked_body(&["\"", "\\"], &["\"\"", "\\\\", "\\\"", "\\'"])
                .prop_map(|b| redacted(format!("\"{b}\""))),
            "\\$[a-z]{1,3}\\$?".prop_map(same),
            prop::sample::select(vec!["[", "]"]).prop_map(|s| same(s.into())),
            // `--` without trailing whitespace is two minus signs.
            (word(), single_quoted_marked(Dialect::MySql))
                .prop_map(|(w, s)| (format!("{w}--{s}"), format!("{w}--?"))),
            // Executable comment: an all-or-nothing version, then ordinary SQL up to `*/`.
            (
                prop::sample::select(vec!["!", "M!"]),
                "[0-9]{0,8}",
                single_quoted_marked(Dialect::MySql)
            )
                .prop_map(|(marker, v, s)| {
                    let (kept, rest) = split_exec_version(&v, marker == "M!");
                    let rest = if rest.is_empty() { "" } else { "?" };
                    (
                        format!("/*{marker}{v} {s} */"),
                        format!("/*{marker}{kept}{rest} ? */"),
                    )
                }),
        ]
        .boxed(),
        Dialect::Sqlite => prop_oneof![
            // Variables, optionally with a `(…)` suffix. The trailing space stops a `(`
            // separator from being read as a suffix.
            (
                prop::sample::select(vec!["$", "@", ":", "#"]),
                "[a-z][a-z0-9]{0,3}\\$?",
                proptest::option::of(body(&[" ", "\t", ")"], &[])),
            )
                .prop_map(|(p, n, suffix)| {
                    let s = match suffix {
                        Some(b) => format!("{p}{n}({b}) "),
                        None => format!("{p}{n} "),
                    };
                    same(s)
                }),
            prop::sample::select(vec!["%", "~"]).prop_map(|s| same(s.into())),
        ]
        .boxed(),
    };
    prop_oneof![8 => common, 2 => specific].boxed()
}

/// A whole statement as `(input, expected obfuscation)`: tokens from [`obfuscation_token`] joined
/// by [`separator`]s.
pub(crate) fn obfuscation_case(dialect: Dialect) -> BoxedStrategy<(String, String)> {
    prop::collection::vec((obfuscation_token(dialect), separator()), 0..12)
        .prop_map(|parts| {
            let mut input = String::new();
            let mut expected = String::new();
            for ((i, e), sep) in parts {
                input.push_str(&i);
                input.push_str(sep);
                expected.push_str(&e);
                expected.push_str(sep);
            }
            (input, expected)
        })
        .boxed()
}

/// A dialect together with an obfuscation case for it.
pub(crate) fn any_obfuscation_case() -> impl Strategy<Value = (Dialect, String, String)> {
    dialect().prop_flat_map(|d| obfuscation_case(d).prop_map(move |(i, e)| (d, i, e)))
}

/// An opener that leaves the rest of the input inside an unterminated literal, identifier or
/// comment in `dialect`. Whatever follows must be redacted.
pub(crate) fn unterminated_opener(dialect: Dialect) -> BoxedStrategy<&'static str> {
    let openers: Vec<&'static str> = match dialect {
        Dialect::Postgres => vec!["'", "\"", "/*", "/* /* */", "$$", "$t$", "E'", "E'a'\n'"],
        Dialect::MySql => vec!["'", "\"", "`", "/*", "/*!'", "/*!12345 \"", "'\\'"],
        Dialect::Sqlite => vec!["'", "\"", "`", "[", "/*", "$v(", "@v::w("],
    };
    prop::sample::select(openers).boxed()
}

/// A token that contains no ASCII digit unless it is a literal the obfuscator must redact, so any
/// digit surviving obfuscation (outside a `MySQL` executable-comment version) is a leak.
pub(crate) fn digit_free_token(dialect: Dialect) -> BoxedStrategy<String> {
    let words = "[a-z_]{1,8}".prop_filter("true/false are literals", |s| {
        !s.eq_ignore_ascii_case("true") && !s.eq_ignore_ascii_case("false")
    });
    let base = prop_oneof![
        words.prop_map(String::from),
        Just("NULL".to_owned()),
        prop::sample::select(vec![",", "=", "(", ")", "+"]).prop_map(String::from),
        number(),
        Just(format!("'{SENTINEL}'")),
    ];
    if dialect == Dialect::MySql {
        prop_oneof![
            4 => base,
            1 => "[0-9]{0,9}".prop_map(|v| format!("/*!{v} 1 */")),
        ]
        .boxed()
    } else {
        base.boxed()
    }
}

/// One token for compaction tests as `(text, is_line_comment)`. Text never starts or ends with
/// whitespace, but literals, identifiers and comments may contain whitespace that must survive.
pub(crate) fn compaction_token(dialect: Dialect) -> BoxedStrategy<(String, bool)> {
    let line = match dialect {
        Dialect::Postgres => body(&[" ", "\t"], &[])
            .prop_map(|b| format!("--{b}"))
            .boxed(),
        Dialect::Sqlite => body(&[" ", "\t"], &["\r"])
            .prop_map(|b| format!("--{b}"))
            .boxed(),
        Dialect::MySql => prop_oneof![
            body(&[" ", "\t"], &[]).prop_map(|b| format!("--\t{b}")),
            body(&[" ", "\t"], &[]).prop_map(|b| format!("#{b}")),
        ]
        .boxed(),
    };
    let string = match dialect {
        Dialect::Postgres | Dialect::Sqlite => body(&["'"], &["''", "\n", "\r"]).boxed(),
        Dialect::MySql => body(&["'", "\\"], &["''", "\\\\", "\\'", "\n"]).boxed(),
    }
    .prop_map(|b| format!("'{b}'"));
    prop_oneof![
        3 => word().prop_map(|w| (w, false)),
        2 => number().prop_map(|n| (n, false)),
        1 => prop::sample::select(vec![",", "=", "(", ")", "?"]).prop_map(|s| (s.to_owned(), false)),
        2 => string.prop_map(|s| (s, false)),
        1 => block_comment(dialect).prop_map(|s| (s, false)),
        1 => quoted_ident(dialect).prop_map(|s| (s, false)),
        1 => line.prop_map(|s| (s, true)),
    ]
    .boxed()
}

/// A whole statement for compaction as `(input, expected)`: tokens separated by random whitespace
/// runs, expected to collapse to one space – or to keep the `\n` that ends a line comment – with
/// trailing spaces trimmed.
pub(crate) fn compaction_case(dialect: Dialect) -> BoxedStrategy<(String, String)> {
    (
        prop::collection::vec((compaction_token(dialect), "[ \t\r\n]{0,4}"), 1..10),
        "[ \t\r\n]{0,3}",
    )
        .prop_map(|(parts, lead)| {
            let mut input = lead;
            let mut expected = String::new();
            let count = parts.len();
            for (idx, ((text, is_line), gap)) in parts.into_iter().enumerate() {
                input.push_str(&text);
                expected.push_str(&text);
                if idx + 1 == count {
                    break;
                }
                if is_line {
                    // The comment must end with a terminator; the rest of the run collapses.
                    input.push('\n');
                    input.push_str(&gap);
                    expected.push('\n');
                } else {
                    // At least one whitespace byte separates tokens so they cannot fuse.
                    input.push(' ');
                    input.push_str(&gap);
                    expected.push(' ');
                }
            }
            (input, expected.trim_end_matches(' ').to_owned())
        })
        .boxed()
}
