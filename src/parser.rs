//! The query language: turns query text into a list of [`Op`]s.

use regex::Regex;
use std::error::Error;
use std::fmt;
use std::sync::LazyLock;

static PROPERTY_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new("^[a-zA-Z_-]+$").unwrap());
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
    /// The keys that contain a name: `name^`. This matches the same keys as
    /// `/name/^`, without a regex.
    Contains(String),
    /// The keys a regex matches: `/regex/`.
    Regex(Regex),
}

impl Pattern {
    pub fn is_match(&self, key: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Contains(name) => key.contains(name.as_str()),
            Self::Regex(regex) => regex.is_match(key),
        }
    }
}

/// Regexes compare equal when they have the same source.
impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Any, Self::Any) => true,
            (Self::Contains(a), Self::Contains(b)) => a == b,
            (Self::Regex(a), Self::Regex(b)) => a.as_str() == b.as_str(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    // Object operations
    Property(String),
    /// A new object holding only the listed entries, in the order listed:
    /// `{a,b.c}`. See [`PickEntry`].
    PropertyPick(Vec<PickEntry>),
    /// The values of the properties whose keys match: `*` or `/regex/`.
    PropertyValues(Pattern),
    /// The keys that match: `*^`, `name^` or `/regex/^`.
    PropertyKeys(Pattern),
    /// `*^`, `name^` or `/regex/^` followed by more steps: a new object with the keys
    /// that match, where each value is the result of running the steps on the
    /// old value, shaped as described by `collect` in the exec module.
    PropertyMap(Pattern, Vec<Op>),
    /// An object step run on every object at any depth, outer objects before
    /// the ones inside them: `**` and the step after it, as in `**.name`.
    Descend(Box<Op>),
    /// `**^` and the steps after it: a new object with the path to every object
    /// at any depth, such as `a.b[0]`, where the steps find something, and
    /// what they find. With no steps, the paths themselves.
    DescendPaths(Vec<Op>),
    // Array operations
    Array,
    ArrayIndex(usize),
    ArraySlice {
        start: Option<isize>,
        stop: Option<isize>,
    },
}

impl Op {
    /// Whether this op can give several results for one value.
    pub fn fans_out(&self) -> bool {
        matches!(
            self,
            Self::PropertyValues(_)
                | Self::PropertyKeys(_)
                | Self::Descend(_)
                | Self::Array
                | Self::ArraySlice { .. }
        )
    }

    /// Whether this op works on an object.
    pub fn is_object_step(&self) -> bool {
        matches!(
            self,
            Self::Property(_)
                | Self::PropertyPick(_)
                | Self::PropertyValues(_)
                | Self::PropertyKeys(_)
                | Self::PropertyMap(..)
        )
    }

    /// Whether this op works on an array.
    pub fn is_array_step(&self) -> bool {
        matches!(
            self,
            Self::Array | Self::ArrayIndex(_) | Self::ArraySlice { .. }
        )
    }
}

/// One entry of a `{...}` pick: the property `name` to read, the `steps` run
/// on its value, and the `key` the result is stored under, which is the last
/// name in the path. `hobbies[0]` reads `hobbies` and runs `[0]` on it under the
/// key `hobbies`, and `work.department` reads `work` and runs `department` on it
/// under the key `department`.
#[derive(Debug, Clone, PartialEq)]
pub struct PickEntry {
    pub key: String,
    pub name: String,
    pub steps: Vec<Op>,
}

/// Parses a query. An empty query has no steps. A query may not start with
/// `.`: write `a.b`, not `.a.b`.
pub fn parse(query: &str) -> Result<Vec<Op>, ParseError> {
    if query.is_empty() {
        return Ok(vec![]);
    }
    parse_chunks(&split_chunks(query)?)
}

fn parse_chunks(chunks: &[&str]) -> Result<Vec<Op>, ParseError> {
    let mut ops = vec![];
    let mut rest = chunks;
    while let Some((&chunk, after)) = rest.split_first() {
        rest = after;
        let op = match chunk {
            // `**` must be followed by an object step, which it runs at every depth.
            "**" => match rest.split_first() {
                Some((&next, after)) => {
                    rest = after;
                    match parse_chunk(next)? {
                        op if op.is_object_step() => Op::Descend(Box::new(op)),
                        _ => return Err(ParseError::InvalidQuery),
                    }
                }
                None => return Err(ParseError::InvalidQuery),
            },
            // `**^` takes all the steps after it, which must start with an
            // object step. A pick builds the object for each path, so it must be last.
            "**^" => {
                let steps = parse_chunks(rest)?;
                let valid = match steps.as_slice() {
                    [] | [Op::PropertyPick(_)] => true,
                    [Op::PropertyPick(_), ..] => false,
                    [first, ..] => first.is_object_step(),
                };
                if !valid {
                    return Err(ParseError::InvalidQuery);
                }
                rest = &[];
                Op::DescendPaths(steps)
            }
            chunk => parse_chunk(chunk)?,
        };
        ops.push(op);
    }
    Ok(nest_after_keys(ops))
}

/// Replaces the first `*^`, `name^` or `/regex/^` that has steps after it with a [`Op::PropertyMap`]
/// holding those steps, and does the same within them. A keys step after `**`
/// is replaced inside its [`Op::Descend`].
fn nest_after_keys(mut ops: Vec<Op>) -> Vec<Op> {
    let is_keys = |op: &Op| match op {
        Op::Descend(op) => matches!(**op, Op::PropertyKeys(_)),
        op => matches!(op, Op::PropertyKeys(_)),
    };
    if let Some(i) = ops.iter().position(is_keys)
        && i + 1 < ops.len()
    {
        let rest = nest_after_keys(ops.split_off(i + 1));
        // After the split, the keys op is the last op.
        match ops.pop() {
            Some(Op::PropertyKeys(pattern)) => ops.push(Op::PropertyMap(pattern, rest)),
            Some(Op::Descend(op)) => {
                if let Op::PropertyKeys(pattern) = *op {
                    ops.push(Op::Descend(Box::new(Op::PropertyMap(pattern, rest))));
                }
            }
            _ => {}
        }
    }
    ops
}

/// Splits a query on `.`, except inside `/regex/` chunks, where `\/` escapes a
/// slash, and inside `{...}`. A `[` also starts a new chunk, so `a[0]` is two
/// chunks, but it may not follow a `.`: `a.[0]` is an error.
fn split_chunks(query: &str) -> Result<Vec<&str>, ParseError> {
    enum State {
        Plain,
        InRegex,
        Escaped,
        RegexClosed,
        RegexKeys,
    }

    let mut chunks = vec![];
    let mut start = 0;
    let mut state = State::Plain;
    // How many `{` are open.
    let mut depth = 0usize;

    for (i, c) in query.char_indices() {
        state = match (state, c) {
            (State::InRegex, '\\') => State::Escaped,
            (State::InRegex, '/') => State::RegexClosed,
            (State::InRegex | State::Escaped, _) => State::InRegex,
            (State::Plain, '{') => {
                depth += 1;
                State::Plain
            }
            (State::Plain, '}') if depth > 0 => {
                depth -= 1;
                State::Plain
            }
            (State::Plain, _) if depth > 0 => State::Plain,
            (_, '.') => {
                chunks.push(&query[start..i]);
                start = i + 1;
                State::Plain
            }
            (_, '[') if i == start && i > 0 => return Err(ParseError::InvalidQuery),
            (_, '[') if i > start => {
                chunks.push(&query[start..i]);
                start = i;
                State::Plain
            }
            (State::RegexClosed, '^') => State::RegexKeys,
            // Nothing may follow a closing `/` except `^` and the next `.` or `[`.
            (State::RegexClosed | State::RegexKeys, _) => return Err(ParseError::InvalidQuery),
            (State::Plain, '/') if i == start => State::InRegex,
            (State::Plain, _) => State::Plain,
        };
    }
    if depth > 0 || matches!(state, State::InRegex | State::Escaped) {
        return Err(ParseError::InvalidQuery);
    }
    chunks.push(&query[start..]);
    Ok(chunks)
}

fn parse_chunk(chunk: &str) -> Result<Op, ParseError> {
    if chunk == "[]" {
        return Ok(Op::Array);
    }
    if let Some(op) = parse_selector(chunk)? {
        return Ok(op);
    }
    if PROPERTY_REGEX.is_match(chunk) {
        return Ok(Op::Property(chunk.to_owned()));
    }
    if let Some(list) = chunk
        .strip_prefix('{')
        .and_then(|rest| rest.strip_suffix('}'))
    {
        return parse_pick_list(list).map(Op::PropertyPick);
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

/// Parses `*` or `/regex/`, which select properties by key, followed by an
/// optional `^` to take their keys instead of their values. Returns `None` if
/// `chunk` is some other kind of step.
fn parse_selector(chunk: &str) -> Result<Option<Op>, ParseError> {
    let (selector, keys) = match chunk.strip_suffix('^') {
        Some(selector) => (selector, true),
        None => (chunk, false),
    };
    let pattern = if selector == "*" {
        Pattern::Any
    } else if let Some(source) = selector
        .strip_prefix('/')
        .and_then(|rest| rest.strip_suffix('/'))
    {
        Pattern::Regex(Regex::new(source).map_err(|_| ParseError::InvalidQuery)?)
    } else if keys && PROPERTY_REGEX.is_match(selector) {
        Pattern::Contains(selector.to_owned())
    } else {
        return Ok(None);
    };
    Ok(Some(if keys {
        Op::PropertyKeys(pattern)
    } else {
        Op::PropertyValues(pattern)
    }))
}

/// Parses the `a,b.c` inside `{a,b.c}`. Two entries may not have the same key.
fn parse_pick_list(list: &str) -> Result<Vec<PickEntry>, ParseError> {
    let mut entries: Vec<PickEntry> = vec![];
    for path in split_top_level_commas(list) {
        for entry in parse_pick_path(&split_chunks(path)?)? {
            if entries.iter().any(|e| e.key == entry.key) {
                return Err(ParseError::InvalidQuery);
            }
            entries.push(entry);
        }
    }
    Ok(entries)
}

/// Splits on the commas that are not inside a nested `{...}`.
fn split_top_level_commas(list: &str) -> Vec<&str> {
    let mut parts = vec![];
    let mut start = 0;
    let mut depth = 0usize;
    for (i, c) in list.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&list[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&list[start..]);
    parts
}

/// Parses one path of a pick, such as `age`, `hobbies[0]` or `last.name`.
/// It starts with a property name, which may be followed by array steps, and
/// may end with another path or a `{...}`. A path gives one entry, keyed by its
/// last name, except one ending in `{...}`, which gives an entry for each entry
/// in the braces: `a.{b,c}` is `a.b,a.c`.
fn parse_pick_path(chunks: &[&str]) -> Result<Vec<PickEntry>, ParseError> {
    let Some((name, mut rest)) = chunks.split_first() else {
        return Err(ParseError::InvalidQuery);
    };
    if !PROPERTY_REGEX.is_match(name) {
        return Err(ParseError::InvalidQuery);
    }
    let name = (*name).to_owned();
    let mut steps = vec![];
    while let Some((chunk, after)) = rest.split_first() {
        let tail = if PROPERTY_REGEX.is_match(chunk) {
            parse_pick_path(rest)?
        } else {
            match parse_chunk(chunk)? {
                Op::PropertyPick(entries) if after.is_empty() => entries,
                op if op.is_array_step() => {
                    steps.push(op);
                    rest = after;
                    continue;
                }
                _ => return Err(ParseError::InvalidQuery),
            }
        };
        // Each entry of the tail reads its property from this one's value.
        return Ok(tail
            .into_iter()
            .map(|entry| {
                let mut path = steps.clone();
                path.push(Op::Property(entry.name));
                path.extend(entry.steps);
                PickEntry {
                    key: entry.key,
                    name: name.clone(),
                    steps: path,
                }
            })
            .collect());
    }
    Ok(vec![PickEntry {
        key: name.clone(),
        name,
        steps,
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prop(name: &str) -> Op {
        Op::Property(name.to_string())
    }

    fn re(pattern: &str) -> Op {
        Op::PropertyValues(Pattern::Regex(Regex::new(pattern).unwrap()))
    }

    #[test]
    fn parse_single_property() {
        assert_eq!(parse("Tim"), Ok(vec![prop("Tim")]));
    }

    #[test]
    fn parse_nested_properties() {
        assert_eq!(parse("Tim.age"), Ok(vec![prop("Tim"), prop("age")]));
    }

    #[test]
    fn parse_wildcard() {
        assert_eq!(parse("*"), Ok(vec![Op::PropertyValues(Pattern::Any)]));
    }

    #[test]
    fn parse_wildcard_then_property() {
        assert_eq!(
            parse("*.age"),
            Ok(vec![Op::PropertyValues(Pattern::Any), prop("age")])
        );
    }

    #[test]
    fn parse_rejects_leading_dot() {
        for q in [
            ".", "..", ".Tim", ".Tim.age", ".[0]", ".*", ".*^", "./a/", ".{a}",
        ] {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn parse_property_with_underscore_and_hyphen() {
        assert_eq!(
            parse("first_name.last-name"),
            Ok(vec![prop("first_name"), prop("last-name")])
        );
    }

    #[test]
    fn parse_empty_query_yields_no_ops() {
        assert_eq!(parse(""), Ok(vec![]));
    }

    #[test]
    fn parse_chunk_wildcard() {
        assert_eq!(parse_chunk("*"), Ok(Op::PropertyValues(Pattern::Any)));
    }

    #[test]
    fn parse_selectors() {
        let regex = |s| Pattern::Regex(Regex::new(s).unwrap());
        assert_eq!(parse("*^"), Ok(vec![Op::PropertyKeys(Pattern::Any)]));
        assert_eq!(parse("/a/"), Ok(vec![Op::PropertyValues(regex("a"))]));
        assert_eq!(parse("/a/^"), Ok(vec![Op::PropertyKeys(regex("a"))]));
        assert_eq!(parse("/a^/"), Ok(vec![Op::PropertyValues(regex("a^"))]));
        assert_eq!(parse("//^"), Ok(vec![Op::PropertyKeys(regex(""))]));
        let contains = |s: &str| Pattern::Contains(s.to_owned());
        assert_eq!(parse("Tim^"), Ok(vec![Op::PropertyKeys(contains("Tim"))]));
        assert_eq!(
            parse("a.first_name-x^.age"),
            Ok(vec![
                prop("a"),
                Op::PropertyMap(contains("first_name-x"), vec![prop("age")])
            ])
        );
    }

    #[test]
    fn pattern_contains_matches_like_regex_of_name() {
        let contains = Pattern::Contains("im".to_owned());
        let regex = Pattern::Regex(Regex::new("im").unwrap());
        for key in ["Tim", "im", "Timothy", "Tom", "", "IM", "i-m"] {
            assert_eq!(contains.is_match(key), regex.is_match(key), "key {key:?}");
        }
    }

    #[test]
    fn pattern_any_matches_every_key() {
        for key in ["", "a", "é", "a.b"] {
            assert!(Pattern::Any.is_match(key), "key {key:?}");
        }
        assert_ne!(Pattern::Any, Pattern::Regex(Regex::new("").unwrap()));
        assert_ne!(Pattern::Any, Pattern::Contains(String::new()));
    }

    #[test]
    fn parse_chunk_property() {
        assert_eq!(parse_chunk("age"), Ok(prop("age")));
    }

    #[test]
    fn parse_chunk_rejects_invalid() {
        for chunk in ["", "1", "a1", "a b", "**", "age^^", "^age", "a1^"] {
            assert!(parse_chunk(chunk).is_err(), "expected error for {chunk:?}");
        }
    }

    #[test]
    fn parse_rejects_invalid_chunk() {
        assert!(parse("foo.$$.bar").is_err());
        assert!(parse("foo..bar").is_err());
    }

    #[test]
    fn parse_slice() {
        assert_eq!(
            parse("[1:-2]"),
            Ok(vec![Op::ArraySlice {
                start: Some(1),
                stop: Some(-2)
            }])
        );
        assert_eq!(
            parse("[:]"),
            Ok(vec![Op::ArraySlice {
                start: None,
                stop: None
            }])
        );
    }

    #[test]
    fn parse_slice_rejects_invalid() {
        for q in ["[::2]", "[1:2:1]", "[1:2:3:4]", "[a:b]", "[1-:2]"] {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn parse_regex() {
        assert_eq!(parse("/^a/"), Ok(vec![re("^a")]));
        assert_eq!(parse("/^a/.b"), Ok(vec![re("^a"), prop("b")]));
        assert_eq!(parse("x.//"), Ok(vec![prop("x"), re("")]));
    }

    #[test]
    fn parse_regex_keeps_dots_and_escaped_slashes() {
        assert_eq!(parse("/a.b/.c"), Ok(vec![re("a.b"), prop("c")]));
        assert_eq!(parse(r"/a\/b/"), Ok(vec![re(r"a\/b")]));
        assert_eq!(parse(r"/a\\/.b"), Ok(vec![re(r"a\\"), prop("b")]));
    }

    #[test]
    fn parse_regex_rejects_invalid() {
        for q in ["/abc", "/a/b", "/a/b/", "a/b/", "/(/", r"/a\/"] {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn split_chunks_on_dots() {
        assert_eq!(split_chunks("a.b.c"), Ok(vec!["a", "b", "c"]));
        assert_eq!(split_chunks("a"), Ok(vec!["a"]));
        assert_eq!(split_chunks("a..b"), Ok(vec!["a", "", "b"]));
    }

    #[test]
    fn split_chunks_keeps_regex_whole() {
        assert_eq!(split_chunks("a./x.y/.b"), Ok(vec!["a", "/x.y/", "b"]));
        assert_eq!(split_chunks(r"/x\/.y/"), Ok(vec![r"/x\/.y/"]));
        assert_eq!(split_chunks(r"/x\./"), Ok(vec![r"/x\./"]));
    }

    #[test]
    fn split_chunks_slash_mid_chunk_is_not_a_regex() {
        // Only a `/` at the start of a chunk opens a regex.
        assert_eq!(split_chunks("a/b.c"), Ok(vec!["a/b", "c"]));
    }

    #[test]
    fn split_chunks_on_brackets() {
        assert_eq!(split_chunks("a[0]"), Ok(vec!["a", "[0]"]));
        assert_eq!(
            split_chunks("a[0][1:].b"),
            Ok(vec!["a", "[0]", "[1:]", "b"])
        );
        assert_eq!(split_chunks("[][0]"), Ok(vec!["[]", "[0]"]));
        assert_eq!(split_chunks("/a/[0]"), Ok(vec!["/a/", "[0]"]));
        assert_eq!(split_chunks("/a/^[0]"), Ok(vec!["/a/^", "[0]"]));
        // Inside a regex, `[` is a character class.
        assert_eq!(split_chunks("/[ab]/"), Ok(vec!["/[ab]/"]));
    }

    #[test]
    fn parse_array_steps_without_dot() {
        assert_eq!(parse("a[0]"), Ok(vec![prop("a"), Op::ArrayIndex(0)]));
        assert_eq!(parse("a[].b"), Ok(vec![prop("a"), Op::Array, prop("b")]));
        assert_eq!(parse("[0]"), Ok(vec![Op::ArrayIndex(0)]));
        for q in ["a[", "a[0", "a[0]b", "a[x]", "a..[0]"] {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn split_chunks_rejects_dot_before_bracket() {
        for q in ["a.[0]", "a.[]", "a[0].[1]", "/a/.[0]", "*^.[0]"] {
            assert!(split_chunks(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn split_chunks_handles_non_ascii() {
        assert_eq!(split_chunks("/é.ü/.ñ"), Ok(vec!["/é.ü/", "ñ"]));
    }

    #[test]
    fn split_chunks_rejects_unterminated_or_trailing_regex() {
        for q in ["/", "/a", r"/a\/", "a./b", "/a/b", "/a//"] {
            assert!(split_chunks(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn parse_array_ops() {
        assert_eq!(parse("[]"), Ok(vec![Op::Array]));
        assert_eq!(parse("[0]"), Ok(vec![Op::ArrayIndex(0)]));
        assert_eq!(
            parse("a[12].b"),
            Ok(vec![prop("a"), Op::ArrayIndex(12), prop("b")])
        );
    }

    #[test]
    fn parse_array_index_rejects_invalid() {
        for q in [
            "[-1]",
            "[a]",
            "[1",
            "1]",
            "[ 1]",
            "[99999999999999999999999]",
        ] {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn pattern_equality_compares_source() {
        let p = |s| Pattern::Regex(Regex::new(s).unwrap());
        assert_eq!(p("^a+$"), p("^a+$"));
        assert_ne!(p("a"), p("b"));
        // Equivalent regexes with different source are not equal.
        assert_ne!(p("a+"), p("aa*"));
    }

    #[test]
    fn parse_pick() {
        let props = |names: &[&str]| pick(names.iter().map(|n| (*n, *n, vec![])).collect());
        assert_eq!(parse("a.{b,c}"), Ok(vec![prop("a"), props(&["b", "c"])]));
        assert_eq!(parse("{b}"), Ok(vec![props(&["b"])]));
        assert_eq!(
            parse("{first_name,last-name}.x"),
            Ok(vec![props(&["first_name", "last-name"]), prop("x")])
        );
    }

    /// A pick of `(key, name, steps)` entries.
    fn pick(entries: Vec<(&str, &str, Vec<Op>)>) -> Op {
        Op::PropertyPick(
            entries
                .into_iter()
                .map(|(key, name, steps)| PickEntry {
                    key: key.to_string(),
                    name: name.to_string(),
                    steps,
                })
                .collect(),
        )
    }

    #[test]
    fn parse_pick_paths_are_keyed_by_last_name() {
        assert_eq!(
            parse("{age,hobbies[0]}"),
            Ok(vec![pick(vec![
                ("age", "age", vec![]),
                ("hobbies", "hobbies", vec![Op::ArrayIndex(0)])
            ])])
        );
        assert_eq!(
            parse("a.{age,last.name}.b"),
            Ok(vec![
                prop("a"),
                pick(vec![
                    ("age", "age", vec![]),
                    ("name", "last", vec![prop("name")])
                ]),
                prop("b")
            ])
        );
        assert_eq!(
            parse("{f[0].n[]}"),
            Ok(vec![pick(vec![(
                "n",
                "f",
                vec![Op::ArrayIndex(0), prop("n"), Op::Array]
            )])])
        );
        assert_eq!(
            parse("{a.b.c}"),
            Ok(vec![pick(vec![("c", "a", vec![prop("b"), prop("c")])])])
        );
    }

    #[test]
    fn parse_pick_paths_ending_in_braces() {
        assert_eq!(parse("{a.{b,c}}"), parse("{a.b,a.c}"));
        assert_eq!(parse("{a.{b,c.d},x,a.c.{e}}"), parse("{a.b,a.c.d,x,a.c.e}"));
        assert_eq!(
            parse("{a[1:].{b}}"),
            Ok(vec![pick(vec![(
                "b",
                "a",
                vec![
                    Op::ArraySlice {
                        start: Some(1),
                        stop: None
                    },
                    prop("b")
                ]
            )])])
        );
    }

    #[test]
    fn parse_pick_allows_shared_names_with_different_keys() {
        for q in ["{a.b,a}", "{a,a.b}", "{a[0],a.b}", "{a.b,a.c}"] {
            assert!(parse(q).is_ok(), "expected ok for {q:?}");
        }
    }

    #[test]
    fn parse_pick_rejects_invalid_paths() {
        for q in [
            "{a.b,a.b}",
            "{a[0],a[1]}",
            "{a.b,c.b}",
            "{b,a.b}",
            "{a.{b,c},c}",
            "{[0]}",
            "{a.[0]}",
            "{a.*}",
            "{a.^}",
            "{a./b/}",
            "{a.{b}.c}",
            "{a.{b}[0]}",
            "{a..b}",
            "{a.}",
            "{.a}",
            "{a.{b}",
            "{a}}",
        ] {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn split_chunks_keeps_braces_whole() {
        assert_eq!(
            split_chunks("a.{b.c,d[0]}.e"),
            Ok(vec!["a", "{b.c,d[0]}", "e"])
        );
        assert_eq!(split_chunks("{a.{b.c}}[0]"), Ok(vec!["{a.{b.c}}", "[0]"]));
    }

    #[test]
    fn parse_pick_rejects_invalid() {
        for q in [
            "{}", "{a,}", "{,a}", "{a b}", "{a, b}", "{a,a}", "{a", "a}", "{a}b", "{1}",
        ] {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn parse_keys() {
        assert_eq!(parse("*^"), Ok(vec![Op::PropertyKeys(Pattern::Any)]));
        for q in ["^", "a.^", "^.a", "*.^", "*^^", "^*"] {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
        assert_eq!(
            parse("a.*^"),
            Ok(vec![prop("a"), Op::PropertyKeys(Pattern::Any)])
        );
        assert!(parse("^^").is_err());
    }

    #[test]
    fn parse_steps_after_keys_nest_in_a_property_map() {
        assert_eq!(
            parse("*^.a"),
            Ok(vec![Op::PropertyMap(Pattern::Any, vec![prop("a")])])
        );
        assert_eq!(
            parse("*.*^.a[]"),
            Ok(vec![
                Op::PropertyValues(Pattern::Any),
                Op::PropertyMap(Pattern::Any, vec![prop("a"), Op::Array])
            ])
        );
    }

    #[test]
    fn parse_nested_keys() {
        assert_eq!(
            parse("*^.*^"),
            Ok(vec![Op::PropertyMap(
                Pattern::Any,
                vec![Op::PropertyKeys(Pattern::Any)]
            )])
        );
        assert_eq!(
            parse("*^.a.*^.b"),
            Ok(vec![Op::PropertyMap(
                Pattern::Any,
                vec![prop("a"), Op::PropertyMap(Pattern::Any, vec![prop("b")])]
            )])
        );
    }

    #[test]
    fn parse_regex_keys() {
        let keys = |pattern: &str| Op::PropertyKeys(Pattern::Regex(Regex::new(pattern).unwrap()));
        assert_eq!(parse("/T.*/^"), Ok(vec![keys("T.*")]));
        assert_eq!(parse("a./x\\//^"), Ok(vec![prop("a"), keys(r"x\/")]));
        assert_eq!(
            parse("/T.*/^.name"),
            Ok(vec![Op::PropertyMap(
                Pattern::Regex(Regex::new("T.*").unwrap()),
                vec![prop("name")]
            )])
        );
    }

    #[test]
    fn parse_regex_keys_rejects_invalid() {
        for q in ["/a/^^", "/a/^b", "/a/^/", "/(/^", "/^"] {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn parse_descend() {
        let descend = |op| Op::Descend(Box::new(op));
        assert_eq!(parse("**.a"), Ok(vec![descend(prop("a"))]));
        assert_eq!(
            parse("x.**.a.b"),
            Ok(vec![prop("x"), descend(prop("a")), prop("b")])
        );
        assert_eq!(parse("**./^a/"), Ok(vec![descend(re("^a"))]));
        assert_eq!(
            parse("**.*^"),
            Ok(vec![descend(Op::PropertyKeys(Pattern::Any))])
        );
        assert_eq!(
            parse("**.a^.b"),
            Ok(vec![descend(Op::PropertyMap(
                Pattern::Contains("a".to_owned()),
                vec![prop("b")]
            ))])
        );
        assert_eq!(
            parse("*^.**.a"),
            Ok(vec![Op::PropertyMap(
                Pattern::Any,
                vec![descend(prop("a"))]
            )])
        );
        assert!(
            matches!(&parse("**.{a,b}").unwrap()[..], [Op::Descend(op)] if matches!(**op, Op::PropertyPick(_)))
        );
    }

    #[test]
    fn parse_descend_paths() {
        assert_eq!(parse("**^"), Ok(vec![Op::DescendPaths(vec![])]));
        assert_eq!(
            parse("x.**^.a.b"),
            Ok(vec![
                prop("x"),
                Op::DescendPaths(vec![prop("a"), prop("b")])
            ])
        );
        assert_eq!(
            parse("**^.a^.b"),
            Ok(vec![Op::DescendPaths(vec![Op::PropertyMap(
                Pattern::Contains("a".to_owned()),
                vec![prop("b")]
            )])])
        );
        assert_eq!(
            parse("*^.**^.a"),
            Ok(vec![Op::PropertyMap(
                Pattern::Any,
                vec![Op::DescendPaths(vec![prop("a")])]
            )])
        );
        for q in [
            "**^[0]",
            "**^.[0]",
            "**^.**.a",
            "**^.**^",
            "**^^",
            "{**^.a}",
            "**^.a.$",
            "**^.{a}.a",
            "**^.{a}[0]",
        ] {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn parse_descend_rejects_invalid() {
        for q in [
            "**", "a.**", "**.**.a", "**[0]", "**.[0]", "**a", "a**", "***", "{**.a}", "{a.**.b}",
            "**.$",
        ] {
            assert!(parse(q).is_err(), "expected error for {q:?}");
        }
    }

    #[test]
    fn parse_error_display() {
        assert_eq!(ParseError::InvalidQuery.to_string(), "invalid query");
    }
}
