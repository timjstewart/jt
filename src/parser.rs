//! The query language: turns query text into a list of [`Op`]s.

use regex::Regex;
use serde_json::Value;
use std::cmp::Ordering;
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

/// A test of a value, at the end of a query. Objects and arrays never pass.
#[derive(Debug, Clone, PartialEq)]
pub enum Comparison {
    /// `=value`: the value's text starts with the given one, or with a `$`,
    /// is exactly it. Numbers, booleans and `null` compare as JSON writes them.
    Equal(Pattern),
    /// `!=value`: the value's text isn't exactly the given one, so `!=2`
    /// keeps `23`.
    NotEqual(String),
    /// `<`, `<=`, `>` or `>=` and a bound: the value's order against the
    /// bound is one of `accepts`.
    Order {
        accepts: Vec<Ordering>,
        bound: Bound,
    },
    /// `=/regex/`: a string the regex matches part of. Other values never
    /// match.
    Matches(Pattern),
    /// `!=/regex/`: a string the regex doesn't match. Other values never
    /// match.
    NotMatches(Pattern),
    /// `!=`, `<`, `<=`, `>` or `>=` with no value yet, as while one is being
    /// typed: every value passes, of any type.
    Any,
}

/// What `<`, `<=`, `>` and `>=` compare with.
#[derive(Debug, Clone, PartialEq)]
pub enum Bound {
    /// A number, which only numbers are compared with.
    Number(f64),
    /// Any other text, which only strings are compared with, by character.
    Text(String),
}

impl Comparison {
    pub fn is_match(&self, value: &Value) -> bool {
        match self {
            Self::Equal(pattern) => scalar_text(value).is_some_and(|text| pattern.is_match(&text)),
            Self::NotEqual(other) => scalar_text(value).is_some_and(|text| text != *other),
            Self::Matches(pattern) => value.as_str().is_some_and(|s| pattern.is_match(s)),
            Self::NotMatches(pattern) => value.as_str().is_some_and(|s| !pattern.is_match(s)),
            Self::Order { accepts, bound } => {
                let order = match (bound, value) {
                    (Bound::Number(bound), Value::Number(n)) => {
                        n.as_f64().and_then(|n| n.partial_cmp(bound))
                    }
                    (Bound::Text(bound), Value::String(s)) => Some(s.as_str().cmp(bound)),
                    _ => None,
                };
                order.is_some_and(|order| accepts.contains(&order))
            }
            Self::Any => true,
        }
    }
}

/// The text `=value` compares with: a string as it is, or a number, boolean or
/// `null` as JSON writes it. Objects and arrays have none, so never match.
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => Some("null".to_owned()),
        Value::Object(_) | Value::Array(_) => None,
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
    /// Keeps a value that passes a comparison at the end of a query, such as
    /// `=value` or `<value`.
    Value(Comparison),
    /// A step that selects properties or elements, followed by `^`, which
    /// leaves its keys or indexes out of the path: the object or array it
    /// selects from is replaced by what it selects.
    Unwrap(Box<Op>),
}

/// Parses a query. An empty query has no steps. A query may not start with
/// `.`, but may end with one, which is ignored so that a query stays valid
/// while it is being typed: `a.` is the same as `a`. For the same reason, an
/// array step left open at the very end is closed: `a[` is the same as `a[]`,
/// `a[3` as `a[3]`, and `a[1:` as `a[1:]`. This doesn't happen before an
/// `=value`, which needs the array step closed first.
///
/// A query may end with a comparison, which keeps only the paths to the
/// values that pass it: see [`parse_comparison`].
pub fn parse(query: &str) -> Result<Vec<Op>, ParseError> {
    let (path, comparison) = match separators(query)?.last() {
        Some(&(i, c)) if is_comparison_start(c) => {
            (&query[..i], Some(parse_comparison(&query[i..])?))
        }
        _ => (query, None),
    };
    let mut ops = parse_query_path(path, comparison.is_none())?;
    if let Some(comparison) = comparison {
        append(&mut ops, Op::Value(comparison));
    }
    Ok(ops)
}

fn is_comparison_start(c: char) -> bool {
    matches!(c, '=' | '!' | '<' | '>')
}

/// Parses a comparison: `=`, `!=`, `<`, `<=`, `>` or `>=`, and the value,
/// which is everything after it.
///
/// - `=value` keeps the strings, numbers, booleans and nulls whose text starts
///   with `value`, or is exactly `value` when it ends in `$`. `!=value` keeps
///   the ones whose text isn't exactly `value`, so `!=2` keeps `23`. A `$` on
///   it changes nothing, and with no value it keeps every value.
/// - `=/regex/` and `!=/regex/` keep the strings the regex does or doesn't
///   match part of. The closing `/` may be left off, as while typing.
/// - `<`, `<=`, `>` and `>=` compare numbers with a value that is a number,
///   and strings with any other value, by character. With no value, they
///   keep every value.
fn parse_comparison(text: &str) -> Result<Comparison, ParseError> {
    let pattern = |value: &str| match value.strip_suffix('$') {
        Some(value) => Pattern::Exact(value.to_owned()),
        None => Pattern::Prefix(value.to_owned()),
    };
    let order = |accepts: &[Ordering], value: &str| {
        if value.is_empty() {
            return Comparison::Any;
        }
        Comparison::Order {
            accepts: accepts.to_vec(),
            bound: match value.parse() {
                Ok(n) => Bound::Number(n),
                Err(_) => Bound::Text(value.to_owned()),
            },
        }
    };
    use Ordering::{Equal, Greater, Less};
    if let Some(value) = text.strip_prefix("!=").or(text.strip_prefix('='))
        && let Some(source) = value.strip_prefix('/')
    {
        let source = match source.strip_suffix('/') {
            Some(closed) if !source.ends_with("\\/") => closed,
            _ => source,
        };
        let regex = Regex::new(source).map_err(|_| ParseError::InvalidQuery)?;
        return Ok(if !text.starts_with('!') {
            Comparison::Matches(Pattern::Regex(regex))
        } else if source.is_empty() {
            // Nothing typed yet, as for `!=`.
            Comparison::Any
        } else {
            Comparison::NotMatches(Pattern::Regex(regex))
        });
    }
    Ok(if let Some(value) = text.strip_prefix("!=") {
        let value = value.strip_suffix('$').unwrap_or(value);
        if text == "!=" {
            Comparison::Any
        } else {
            Comparison::NotEqual(value.to_owned())
        }
    } else if let Some(value) = text.strip_prefix("<=") {
        order(&[Less, Equal], value)
    } else if let Some(value) = text.strip_prefix(">=") {
        order(&[Greater, Equal], value)
    } else if let Some(value) = text.strip_prefix('<') {
        order(&[Less], value)
    } else if let Some(value) = text.strip_prefix('>') {
        order(&[Greater], value)
    } else if let Some(value) = text.strip_prefix('=') {
        Comparison::Equal(pattern(value))
    } else {
        return Err(ParseError::InvalidQuery);
    })
}

/// Parses the path of a query, before any comparison. An array step left open
/// is only closed when the path ends the query: `a[=x` is invalid.
fn parse_query_path(path: &str, ends_query: bool) -> Result<Vec<Op>, ParseError> {
    let text = match path.strip_suffix('.') {
        Some(rest) if !rest.is_empty() => rest,
        _ => path,
    };
    if text.is_empty() {
        return Ok(vec![]);
    }
    if ends_query
        && let Some(open) = text.rfind('[')
        && !text[open..].contains(']')
    {
        return parse_path(&format!("{text}]"));
    }
    parse_path(text)
}

/// Adds `op` to the end of every path in `ops`, inside any `{...}` at the end.
fn append(ops: &mut Vec<Op>, op: Op) {
    match ops.last_mut() {
        Some(Op::Branches(branches)) => {
            for branch in branches {
                append(branch, op.clone());
            }
        }
        _ => ops.push(op),
    }
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
/// The first `=`, `!`, `<` or `>` outside them ends the scan, since it starts
/// a comparison and what follows is its value: it is the last position given.
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
            State::RegexClosed
                if !matches!(c, '.' | '[' | ',' | '}' | '^') && !is_comparison_start(c) =>
            {
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
            c if depth == 0 && is_comparison_start(c) => {
                found.push((i, c));
                return Ok(found);
            }
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
            // A comparison can only end a whole query, not a path in braces.
            c if is_comparison_start(c) => return Err(ParseError::InvalidQuery),
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
        if is_comparison_start(c) {
            return Err(ParseError::InvalidQuery);
        }
        if c == ',' {
            parts.push(&list[start..i]);
            start = i + 1;
        }
    }
    parts.push(&list[start..]);
    Ok(parts)
}

fn parse_chunk(chunk: &str) -> Result<Op, ParseError> {
    // A regex may end in `^`, so `/a^/` has no `^` step: it ends in `/`.
    if let Some(step) = chunk.strip_suffix('^') {
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
        assert_invalid(&["$", "a$$", "$a", "a$b", "a b", "^"]);
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
            "/abc", "/a/b", "/a/b/", "a/b/", "/(/", r"/a\/", "/a//", "/a/^^",
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
        assert_eq!(parse("a^["), parse("a^[]"));
        assert_eq!(parse("a[3"), parse("a[3]"));
        assert_eq!(parse("a[12"), parse("a[12]"));
        assert_eq!(parse("a[1:"), parse("a[1:]"));
        assert_eq!(parse("a[-2:"), parse("a[-2:]"));
        assert_eq!(parse("a[:-1"), parse("a[:-1]"));
        // Only at the very end, and not inside a regex or braces.
        assert_invalid(&[
            "a[.b", "a[[", "a.[", "/a[", "{a[", "{a[}", "a[^", "a[3.b", "a[x", "a[-", "a[1:-",
        ]);
    }

    #[test]
    fn parse_value() {
        let value = |s: &str| Op::Value(Comparison::Equal(Pattern::Prefix(s.to_owned())));
        let exact_value = |s: &str| Op::Value(Comparison::Equal(Pattern::Exact(s.to_owned())));
        assert_eq!(
            ops("*.car.**.name=He"),
            vec![
                Op::Keys(Pattern::Any),
                name("car"),
                Op::Descend,
                name("name"),
                value("He")
            ]
        );
        assert_eq!(ops("a=Herbie$"), vec![name("a"), exact_value("Herbie")]);
        // Everything after the `=` is the value.
        assert_eq!(ops("a=1.5"), vec![name("a"), value("1.5")]);
        assert_eq!(ops("a=x=y{[/"), vec![name("a"), value("x=y{[/")]);
        assert_eq!(ops("a="), vec![name("a"), value("")]);
        assert_eq!(ops("="), vec![value("")]);
        // The conveniences for typing still apply to the path.
        assert_eq!(ops("a.=x"), ops("a=x"));
        assert_eq!(ops("a[]=x"), vec![name("a"), Op::Array, value("x")]);
        assert_eq!(ops("a^=x"), vec![unwrap(name("a")), value("x")]);
        // A regex may hold an `=`, and a value goes into every path in braces.
        assert_eq!(ops("/a=b/"), vec![re("a=b")]);
        assert_eq!(ops("/a/=b"), vec![re("a"), value("b")]);
        assert_eq!(
            ops("{a,b.{c,d}}=x"),
            vec![Op::Branches(vec![
                vec![name("a"), value("x")],
                vec![
                    name("b"),
                    Op::Branches(vec![
                        vec![name("c"), value("x")],
                        vec![name("d"), value("x")]
                    ])
                ]
            ])]
        );
        // An array step must be closed before the `=`.
        assert_invalid(&[
            "{a=1}", "{a=1,b}", ".=x", "**=x", "a[=x", "a[0=x", "a[1:=x", "a[=x]",
        ]);
    }

    #[test]
    fn parse_comparisons() {
        use Ordering::{Equal, Greater, Less};
        let last = |q: &str| match ops(q).pop() {
            Some(Op::Value(comparison)) => comparison,
            op => panic!("{q:?} ends in {op:?}"),
        };
        let order = |accepts: &[Ordering], bound| Comparison::Order {
            accepts: accepts.to_vec(),
            bound,
        };
        let n = Bound::Number;
        let text = |s: &str| Bound::Text(s.to_owned());
        assert_eq!(last("a!=He"), Comparison::NotEqual("He".into()));
        assert_eq!(last("a!=He$"), Comparison::NotEqual("He".into()));
        assert_eq!(last("a!=$"), Comparison::NotEqual(String::new()));
        assert_eq!(last("a<5"), order(&[Less], n(5.0)));
        assert_eq!(last("a<=-1.5"), order(&[Less, Equal], n(-1.5)));
        assert_eq!(last("a>1e3"), order(&[Greater], n(1000.0)));
        assert_eq!(last("a>=m"), order(&[Greater, Equal], text("m")));
        for q in ["a!=", "a<", "a<=", "a>", "a>="] {
            assert_eq!(last(q), Comparison::Any, "{q:?}");
        }
        // The first operator starts the comparison, and the rest is its value.
        assert_eq!(last("a<=>x"), order(&[Less, Equal], text(">x")));
        assert_eq!(
            last("a=<5"),
            Comparison::Equal(Pattern::Prefix("<5".into()))
        );
        assert_eq!(last("/a<b/>c"), order(&[Greater], text("c")));
        assert_eq!(ops("a^<5")[0], unwrap(name("a")));
        let regex = |s: &str| Pattern::Regex(Regex::new(s).unwrap());
        assert_eq!(last("a=/^He/"), Comparison::Matches(regex("^He")));
        assert_eq!(last("a!=/^He/"), Comparison::NotMatches(regex("^He")));
        // The closing `/` may be left off, and `\/` is a slash in the regex.
        assert_eq!(last("a=/^He"), Comparison::Matches(regex("^He")));
        assert_eq!(last(r"a=/a\//"), Comparison::Matches(regex(r"a\/")));
        assert_eq!(last(r"a=/a\/"), Comparison::Matches(regex(r"a\/")));
        assert_eq!(last("a=/"), Comparison::Matches(regex("")));
        assert_eq!(last("a!=/"), Comparison::Any);
        assert_eq!(last("a!=//"), Comparison::Any);
        assert_invalid(&[
            "a!", "a!x", "a!<5", "{a<1}", "a[<1", "a!!=1", "a=/(/", "a!=/[a",
        ]);
    }

    #[test]
    fn comparisons_match() {
        let matches = |q: &str, value: Value| match ops(q).pop() {
            Some(Op::Value(comparison)) => comparison.is_match(&value),
            op => panic!("{q:?} ends in {op:?}"),
        };
        use serde_json::json;
        // A regex matches part of a string, and never matches other values.
        assert!(matches("a=/rb/", json!("Herbie")));
        assert!(!matches("a=/^rb/", json!("Herbie")));
        assert!(!matches("a=/2/", json!(23)));
        assert!(!matches("a=/n/", json!(null)));
        assert!(matches("a!=/^He/", json!("Kevin")));
        assert!(!matches("a!=/^He/", json!("Herbie")));
        assert!(!matches("a!=/x/", json!(23)));
        assert!(!matches("a!=/x/", json!(true)));
        // `!=` is exact, unlike `=`.
        assert!(matches("a!=He", json!("Kevin")));
        assert!(matches("a!=He", json!("Herbie")));
        assert!(!matches("a!=He", json!("He")));
        assert!(!matches("a!=He$", json!("He")));
        assert!(!matches("a!=2", json!(2)));
        assert!(matches("a!=2", json!(23)));
        assert!(matches("a!=$", json!("x")));
        assert!(!matches("a!=$", json!("")));
        assert!(matches("a!=x", json!(null)));
        assert!(!matches("a!=null", json!(null)));
        assert!(matches("a=null", json!(null)));
        assert!(matches("a=null$", json!(null)));
        assert!(matches("a=nu", json!(null)));
        assert!(!matches("a=null", json!("x")));
        // `null` doesn't order.
        assert!(!matches("a<z", json!(null)));
        assert!(!matches("a!=x", json!({})));
        assert!(matches("a<5", json!(4.5)));
        assert!(!matches("a<5", json!(5)));
        assert!(matches("a<=5", json!(5)));
        assert!(matches("a>=5", json!(5)));
        assert!(matches("a>5", json!(10)));
        // Numbers compare as numbers, not as text.
        assert!(!matches("a<5", json!(10)));
        // A number bound doesn't compare with strings, nor text with numbers.
        assert!(!matches("a<5", json!("1")));
        assert!(!matches("a>a", json!(1)));
        assert!(matches("a>b", json!("c")));
        assert!(matches("a<b", json!("abc")));
        assert!(!matches("a<b", json!(true)));
        // With no bound, every value matches.
        for value in [
            json!(1),
            json!("s"),
            json!(true),
            json!(null),
            json!({}),
            json!([]),
        ] {
            assert!(matches("a<", value.clone()), "{value}");
            assert!(matches("a>=", value.clone()), "{value}");
            assert!(matches("a!=", value.clone()), "{value}");
        }
    }

    #[test]
    fn parse_unwrap() {
        assert_eq!(ops("Fred.age^"), vec![name("Fred"), unwrap(name("age"))]);
        assert_eq!(
            ops("**.car^.cou"),
            vec![Op::Descend, unwrap(name("car")), name("cou")]
        );
        assert_eq!(
            ops("a$^.*^./b/^[0]^[1:]^"),
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
            ops("{a^,b}"),
            vec![Op::Branches(vec![vec![unwrap(name("a"))], vec![name("b")]])]
        );
        // A `^` inside a regex is part of it.
        assert_eq!(ops("/a^/"), vec![re("a^")]);
        assert_invalid(&[
            "^", "a^^", ".^", "a.^", "^a", "a^b", "**^.a", "{a}^", "[]^^",
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
        assert_invalid(&["**", "a.**", "**.**.a", "**a", "a**", "***", "**.", "**^"]);
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
