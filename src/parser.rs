//! The query language: turns query text into a list of [`Op`]s.

use regex::Regex;
use std::error::Error;
use std::fmt;
use std::sync::LazyLock;

static NAME_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new("^[a-zA-Z0-9_-]+$").unwrap());
static EXACT_NAME_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([a-zA-Z0-9_-]+)\$$").unwrap());
static ARRAY_INDEX_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[([0-9]+)\]$").unwrap());
static ARRAY_SLICE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(-?[0-9]+)?:(-?[0-9]+)?\]$").unwrap());

/// The query text could not be parsed.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    InvalidQuery,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::InvalidQuery => f.write_str("invalid query"),
        }
    }
}

impl Error for ParseError {}

/// Selects the properties of an object by key.
#[derive(Debug, Clone)]
pub enum Pattern {
    /// Every key: `*`.
    Any,
    /// The keys that start with a name: `name`.
    Prefix(String),
    /// The key that is exactly a name: `name$`.
    Exact(String),
    /// The keys a regex matches: `/regex/`.
    Regex(Regex),
}

impl Pattern {
    pub fn is_match(&self, key: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Prefix(name) => key.starts_with(name.as_str()),
            Self::Exact(name) => key == name,
            Self::Regex(regex) => regex.is_match(key),
        }
    }
}

/// Regexes compare equal when they have the same source.
impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Any, Self::Any) => true,
            (Self::Prefix(a), Self::Prefix(b)) | (Self::Exact(a), Self::Exact(b)) => a == b,
            (Self::Regex(a), Self::Regex(b)) => a.as_str() == b.as_str(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// The properties of an object whose keys match: `name`, `name$`, `*` or
    /// `/regex/`.
    Keys(Pattern),
    /// Several paths, each run on the same value: `{a,b.c}`. The steps after
    /// the braces are added to the end of each path when parsing, so this is
    /// always the last op.
    Branches(Vec<Vec<Op>>),
    /// The steps after it, run on the value and on every value inside it, at
    /// any depth: `**`.
    Descend,
    /// Every element of an array: `[]`.
    Array,
    /// One element of an array: `[n]`.
    ArrayIndex(usize),
    /// A range of elements of an array: `[start:stop]`.
    ArraySlice {
        start: Option<isize>,
        stop: Option<isize>,
    },
    /// A step that selects properties or elements, followed by `!`, which
    /// leaves its keys or indexes out of the path: the object or array it
    /// selects from is replaced by what it selects.
    Unwrap(Box<Op>),
}

/// Parses a query. An empty query has no steps. A query may not start with
/// `.`, but may end with one, which is ignored so that a query stays valid
/// while it is being typed: `a.` is the same as `a`. For the same reason, an
/// array step left open at the very end is closed: `a[` is the same as `a[]`,
/// `a[3` as `a[3]`, and `a[1:` as `a[1:]`.
pub fn parse(query: &str) -> Result<Vec<Op>, ParseError> {
    let text = match query.strip_suffix('.') {
        Some(rest) if !rest.is_empty() => rest,
        _ => query,
    };
    if text.is_empty() {
        return Ok(vec![]);
    }
    if let Some(open) = text.rfind('[')
        && !text[open..].contains(']')
    {
        return parse_path(&format!("{text}]"));
    }
    parse_path(text)
}

/// Parses a path of steps separated by `.`, such as `a.b[0]`.
fn parse_path(text: &str) -> Result<Vec<Op>, ParseError> {
    parse_chunks(&split_chunks(text)?)
}

fn parse_chunks(chunks: &[&str]) -> Result<Vec<Op>, ParseError> {
    let mut ops = vec![];
    for (i, chunk) in chunks.iter().enumerate() {
        match parse_chunk(chunk)? {
            // The steps after a `{...}` continue each of its paths.
            Op::Branches(branches) => {
                let rest = parse_chunks(&chunks[i + 1..])?;
                ops.push(Op::Branches(
                    branches
                        .into_iter()
                        .map(|mut branch| {
                            branch.extend(rest.iter().cloned());
                            branch
                        })
                        .collect(),
                ));
                return Ok(ops);
            }
            // `**` needs a step after it, and two in a row would do nothing more.
            Op::Descend if matches!(ops.last(), Some(Op::Descend)) || i + 1 == chunks.len() => {
                return Err(ParseError::InvalidQuery);
            }
            op => ops.push(op),
        }
    }
    Ok(ops)
}

/// The positions of the `.`, `[` and `,` characters in `text` that are not
/// inside a `{...}` or a `/regex/`. A regex starts with a `/` at the start of a
/// step, and in it `\/` escapes a slash. Only the end of a step may follow it.
fn separators(text: &str) -> Result<Vec<(usize, char)>, ParseError> {
    enum State {
        Plain,
        InRegex,
        Escaped,
        RegexClosed,
    }

    let mut found = vec![];
    let mut state = State::Plain;
    // How many `{` are open.
    let mut depth = 0usize;
    let mut step_start = true;
    for (i, c) in text.char_indices() {
        match state {
            State::InRegex => {
                state = match c {
                    '\\' => State::Escaped,
                    '/' => State::RegexClosed,
                    _ => State::InRegex,
                };
                continue;
            }
            State::Escaped => {
                state = State::InRegex;
                continue;
            }
            State::RegexClosed if !matches!(c, '.' | '[' | ',' | '}' | '!') => {
                return Err(ParseError::InvalidQuery);
            }
            State::RegexClosed | State::Plain => state = State::Plain,
        }
        if c == '/' && step_start {
            state = State::InRegex;
            step_start = false;
            continue;
        }
        step_start = matches!(c, '.' | ',' | '{');
        match c {
            '{' => depth += 1,
            '}' if depth == 0 => return Err(ParseError::InvalidQuery),
            '}' => depth -= 1,
            '.' | '[' | ',' if depth == 0 => found.push((i, c)),
            _ => {}
        }
    }
    if depth > 0 || matches!(state, State::InRegex | State::Escaped) {
        return Err(ParseError::InvalidQuery);
    }
    Ok(found)
}

/// Splits a path on `.`, except inside `/regex/` and `{...}`. A `[` also
/// starts a new chunk, so `a[0]` is two chunks, but it may not follow a `.`:
/// `a.[0]` is an error.
fn split_chunks(text: &str) -> Result<Vec<&str>, ParseError> {
    let mut chunks = vec![];
    let mut start = 0;
    for (i, c) in separators(text)? {
        match c {
            '.' => {
                chunks.push(&text[start..i]);
                start = i + 1;
            }
            '[' if i == start && i > 0 => return Err(ParseError::InvalidQuery),
            '[' if i > start => {
                chunks.push(&text[start..i]);
                start = i;
            }
            _ => {}
        }
    }
    chunks.push(&text[start..]);
    Ok(chunks)
}

/// Splits the `a,b.c` inside `{a,b.c}` on the commas that are not inside a
/// nested `{...}` or a `/regex/`.
fn split_commas(list: &str) -> Result<Vec<&str>, ParseError> {
    let mut parts = vec![];
    let mut start = 0;
    for (i, c) in separators(list)? {
        if c == ',' {
            parts.push(&list[start..i]);
            start = i + 1;
        }
    }
    parts.push(&list[start..]);
    Ok(parts)
}

fn parse_chunk(chunk: &str) -> Result<Op, ParseError> {
    // A regex may end in `!`, so `/a!/` has no `!` step: it ends in `/`.
    if let Some(step) = chunk.strip_suffix('!') {
        return match parse_chunk(step)? {
            op @ (Op::Keys(_) | Op::Array | Op::ArrayIndex(_) | Op::ArraySlice { .. }) => {
                Ok(Op::Unwrap(Box::new(op)))
            }
            _ => Err(ParseError::InvalidQuery),
        };
    }
    match chunk {
        "**" => return Ok(Op::Descend),
        "*" => return Ok(Op::Keys(Pattern::Any)),
        "[]" => return Ok(Op::Array),
        _ => {}
    }
    if NAME_REGEX.is_match(chunk) {
        return Ok(Op::Keys(Pattern::Prefix(chunk.to_owned())));
    }
    if let Some(caps) = EXACT_NAME_REGEX.captures(chunk) {
        return Ok(Op::Keys(Pattern::Exact(caps[1].to_owned())));
    }
    if let Some(source) = chunk
        .strip_prefix('/')
        .and_then(|rest| rest.strip_suffix('/'))
    {
        let regex = Regex::new(source).map_err(|_| ParseError::InvalidQuery)?;
        return Ok(Op::Keys(Pattern::Regex(regex)));
    }
    if let Some(list) = chunk
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    {
        let branches = split_commas(list)?
            .into_iter()
            .map(parse_path)
            .collect::<Result<_, _>>()?;
        return Ok(Op::Branches(branches));
    }
    if let Some(caps) = ARRAY_INDEX_REGEX.captures(chunk) {
        let n = caps[1].parse().map_err(|_| ParseError::InvalidQuery)?;
        return Ok(Op::ArrayIndex(n));
    }
    if let Some(caps) = ARRAY_SLICE_REGEX.captures(chunk) {
        let bound = |i| {
            caps.get(i)
                .map(|m| m.as_str().parse().map_err(|_| ParseError::InvalidQuery))
                .transpose()
        };
        return Ok(Op::ArraySlice {
            start: bound(1)?,
            stop: bound(2)?,
        });
    }
    Err(ParseError::InvalidQuery)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(name: &str) -> Op {
        Op::Keys(Pattern::Prefix(name.to_owned()))
    }

    fn exact(name: &str) -> Op {
        Op::Keys(Pattern::Exact(name.to_owned()))
    }

    fn re(pattern: &str) -> Op {
        Op::Keys(Pattern::Regex(Regex::new(pattern).unwrap()))
    }

    fn ops(query: &str) -> Vec<Op> {
        parse(query).unwrap()
    }

    fn unwrap(op: Op) -> Op {
        Op::Unwrap(Box::new(op))
    }

    fn assert_invalid(queries: &[&str]) {
        for q in queries {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn parse_names() {
        assert_eq!(ops("Tim"), vec![name("Tim")]);
        assert_eq!(ops("Tim$"), vec![exact("Tim")]);
        assert_eq!(ops("Tim.age$"), vec![name("Tim"), exact("age")]);
        assert_eq!(
            ops("first_name.last-name.user_1"),
            vec![name("first_name"), name("last-name"), name("user_1")]
        );
        assert_invalid(&["$", "a$$", "$a", "a$b", "a b", "a^", "^"]);
    }

    #[test]
    fn parse_empty_query_has_no_steps() {
        assert_eq!(ops(""), vec![]);
    }

    #[test]
    fn parse_wildcard_and_regex() {
        assert_eq!(ops("*"), vec![Op::Keys(Pattern::Any)]);
        assert_eq!(ops("*.age"), vec![Op::Keys(Pattern::Any), name("age")]);
        assert_eq!(ops("/^a/.b"), vec![re("^a"), name("b")]);
        assert_eq!(ops("x.//"), vec![name("x"), re("")]);
    }

    #[test]
    fn parse_regex_keeps_dots_and_escaped_slashes() {
        assert_eq!(ops("/a.b/.c"), vec![re("a.b"), name("c")]);
        assert_eq!(ops(r"/a\/b/"), vec![re(r"a\/b")]);
        assert_eq!(ops(r"/a\\/.b"), vec![re(r"a\\"), name("b")]);
        assert_eq!(ops("/é.ü/.n"), vec![re("é.ü"), name("n")]);
        assert_eq!(ops("/[ab]/[0]"), vec![re("[ab]"), Op::ArrayIndex(0)]);
    }

    #[test]
    fn parse_regex_rejects_invalid() {
        assert_invalid(&[
            "/abc", "/a/b", "/a/b/", "a/b/", "/(/", r"/a\/", "/a//", "/a/^",
        ]);
    }

    #[test]
    fn parse_rejects_leading_dot_and_empty_steps() {
        assert_invalid(&[
            ".", "..", ".Tim", ".[0]", ".*", "./a/", ".{a}", "a..b", "a..", "{a.}",
        ]);
    }

    #[test]
    fn parse_ignores_a_trailing_dot() {
        assert_eq!(parse("Tim."), parse("Tim"));
        assert_eq!(parse("*.a[0]."), parse("*.a[0]"));
    }

    #[test]
    fn parse_reads_a_trailing_bracket_as_every_element() {
        assert_eq!(parse("a["), parse("a[]"));
        assert_eq!(parse("["), parse("[]"));
        assert_eq!(parse("a[0]["), parse("a[0][]"));
        assert_eq!(parse("a!["), parse("a![]"));
        assert_eq!(parse("a[3"), parse("a[3]"));
        assert_eq!(parse("a[12"), parse("a[12]"));
        assert_eq!(parse("a[1:"), parse("a[1:]"));
        assert_eq!(parse("a[-2:"), parse("a[-2:]"));
        assert_eq!(parse("a[:-1"), parse("a[:-1]"));
        // Only at the very end, and not inside a regex or braces.
        assert_invalid(&[
            "a[.b", "a[[", "a.[", "/a[", "{a[", "{a[}", "a[!", "a[3.b", "a[x", "a[-", "a[1:-",
        ]);
    }

    #[test]
    fn parse_unwrap() {
        assert_eq!(ops("Fred.age!"), vec![name("Fred"), unwrap(name("age"))]);
        assert_eq!(
            ops("**.car!.cou"),
            vec![Op::Descend, unwrap(name("car")), name("cou")]
        );
        assert_eq!(
            ops("a$!.*!./b/![0]![1:]!"),
            vec![
                unwrap(exact("a")),
                unwrap(Op::Keys(Pattern::Any)),
                unwrap(re("b")),
                unwrap(Op::ArrayIndex(0)),
                unwrap(Op::ArraySlice {
                    start: Some(1),
                    stop: None
                })
            ]
        );
        assert_eq!(
            ops("{a!,b}"),
            vec![Op::Branches(vec![vec![unwrap(name("a"))], vec![name("b")]])]
        );
        // A `!` inside a regex is part of it.
        assert_eq!(ops("/a!/"), vec![re("a!")]);
        assert_invalid(&[
            "!", "a!!", ".!", "a.!", "!a", "a!b", "**!.a", "{a}!", "[]!!",
        ]);
    }

    #[test]
    fn parse_array_steps() {
        assert_eq!(ops("[]"), vec![Op::Array]);
        assert_eq!(ops("a[0]"), vec![name("a"), Op::ArrayIndex(0)]);
        assert_eq!(ops("a$[0]"), vec![exact("a"), Op::ArrayIndex(0)]);
        assert_eq!(ops("a[].b"), vec![name("a"), Op::Array, name("b")]);
        assert_eq!(
            ops("a[0][1:-2]"),
            vec![
                name("a"),
                Op::ArrayIndex(0),
                Op::ArraySlice {
                    start: Some(1),
                    stop: Some(-2)
                }
            ]
        );
        assert_eq!(
            ops("[:]"),
            vec![Op::ArraySlice {
                start: None,
                stop: None
            }]
        );
        assert_invalid(&[
            "a[0]b",
            "a[x]",
            "a.[0]",
            "a[0].[1]",
            "[-1]",
            "[ 1]",
            "[::2]",
            "[1:2:3]",
            "[99999999999999999999999]",
        ]);
    }

    #[test]
    fn parse_branches_take_the_steps_after_them() {
        assert_eq!(
            ops("a.{b,c.d}"),
            vec![
                name("a"),
                Op::Branches(vec![vec![name("b")], vec![name("c"), name("d")]])
            ]
        );
        assert_eq!(
            ops("{b,c[0]}[1][2].x$"),
            vec![Op::Branches(vec![
                vec![name("b"), Op::ArrayIndex(1), Op::ArrayIndex(2), exact("x")],
                vec![
                    name("c"),
                    Op::ArrayIndex(0),
                    Op::ArrayIndex(1),
                    Op::ArrayIndex(2),
                    exact("x")
                ]
            ])]
        );
        assert_eq!(
            ops("{a.{b,c}}"),
            vec![Op::Branches(vec![vec![
                name("a"),
                Op::Branches(vec![vec![name("b")], vec![name("c")]])
            ]])]
        );
        // Any steps can be in a branch, including regexes with commas and braces.
        assert_eq!(
            ops("{/a,}/,**.b,[0]}"),
            vec![Op::Branches(vec![
                vec![re("a,}")],
                vec![Op::Descend, name("b")],
                vec![Op::ArrayIndex(0)]
            ])]
        );
        assert_invalid(&[
            "{}", "{a,}", "{,a}", "{a, b}", "{a", "a}", "{a}b", "{a}}", "{a.{b}",
        ]);
    }

    #[test]
    fn parse_descend() {
        assert_eq!(ops("**.a"), vec![Op::Descend, name("a")]);
        assert_eq!(
            ops("x.**.a.b"),
            vec![name("x"), Op::Descend, name("a"), name("b")]
        );
        assert_eq!(ops("**[0]"), vec![Op::Descend, Op::ArrayIndex(0)]);
        assert_invalid(&["**", "a.**", "**.**.a", "**a", "a**", "***", "**.", "**!"]);
    }

    #[test]
    fn pattern_matches() {
        let cases = [
            (
                Pattern::Prefix("Tim".into()),
                ["Tim", "Timothy"],
                ["xTim", "tim"],
            ),
            (
                Pattern::Exact("Tim".into()),
                ["Tim", "Tim"],
                ["Timothy", "Ti"],
            ),
            (Pattern::Any, ["", "a.b"], ["", ""]),
        ];
        for (pattern, matches, misses) in cases {
            for key in matches {
                assert!(pattern.is_match(key), "{pattern:?} on {key:?}");
            }
            if pattern != Pattern::Any {
                for key in misses {
                    assert!(!pattern.is_match(key), "{pattern:?} on {key:?}");
                }
            }
        }
    }

    #[test]
    fn pattern_equality_compares_regex_source() {
        let p = |s| Pattern::Regex(Regex::new(s).unwrap());
        assert_eq!(p("^a+$"), p("^a+$"));
        assert_ne!(p("a+"), p("aa*"));
        assert_ne!(Pattern::Prefix("a".into()), Pattern::Exact("a".into()));
    }

    #[test]
    fn parse_error_display() {
        assert_eq!(ParseError::InvalidQuery.to_string(), "invalid query");
    }
}
