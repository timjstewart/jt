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
    StepAfterKeys,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::InvalidQuery => f.write_str("invalid query"),
            Self::StepAfterKeys => f.write_str("`^` must be the last step"),
        }
    }
}

impl Error for ParseError {}

/// A compiled regex that compares equal to another with the same pattern.
#[derive(Debug)]
pub struct Pattern(Regex);

impl Pattern {
    pub fn is_match(&self, haystack: &str) -> bool {
        self.0.is_match(haystack)
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

#[derive(Debug, PartialEq)]
pub enum Op {
    // Object operations
    Property(String),
    PropertyWildcard,
    PropertyKeys,
    PropertyRegex(Pattern),
    // Array operations
    Array,
    ArrayIndex(usize),
    ArraySlice {
        start: Option<isize>,
        stop: Option<isize>,
    },
}

pub fn parse(query: &str) -> Result<Vec<Op>, ParseError> {
    let query = query.strip_prefix('.').unwrap_or(query);
    if query.is_empty() {
        return Ok(vec![]);
    }
    let ops: Vec<Op> = split_chunks(query)?
        .into_iter()
        .map(parse_chunk)
        .collect::<Result<_, _>>()?;
    // Keys are strings, and no step applies to a string.
    if let Some((_, before_last)) = ops.split_last()
        && before_last.contains(&Op::PropertyKeys)
    {
        return Err(ParseError::StepAfterKeys);
    }
    Ok(ops)
}

/// Splits a query on `.`, except inside `/regex/` chunks, where `\/` escapes a slash.
fn split_chunks(query: &str) -> Result<Vec<&str>, ParseError> {
    enum State {
        Plain,
        InRegex,
        Escaped,
        RegexClosed,
    }

    let mut chunks = vec![];
    let mut start = 0;
    let mut state = State::Plain;

    for (i, c) in query.char_indices() {
        state = match (state, c) {
            (State::InRegex, '\\') => State::Escaped,
            (State::InRegex, '/') => State::RegexClosed,
            (State::InRegex | State::Escaped, _) => State::InRegex,
            (_, '.') => {
                chunks.push(&query[start..i]);
                start = i + 1;
                State::Plain
            }
            // Nothing may follow a closing `/` except the next `.`.
            (State::RegexClosed, _) => return Err(ParseError::InvalidQuery),
            (State::Plain, '/') if i == start => State::InRegex,
            (State::Plain, _) => State::Plain,
        };
    }
    if matches!(state, State::InRegex | State::Escaped) {
        return Err(ParseError::InvalidQuery);
    }
    chunks.push(&query[start..]);
    Ok(chunks)
}

fn parse_chunk(chunk: &str) -> Result<Op, ParseError> {
    match chunk {
        "*" => return Ok(Op::PropertyWildcard),
        "^" => return Ok(Op::PropertyKeys),
        "[]" => return Ok(Op::Array),
        _ => {}
    }
    if let Some(pattern) = chunk
        .strip_prefix('/')
        .and_then(|rest| rest.strip_suffix('/'))
    {
        let regex = Regex::new(pattern).map_err(|_| ParseError::InvalidQuery)?;
        return Ok(Op::PropertyRegex(Pattern(regex)));
    }
    if PROPERTY_REGEX.is_match(chunk) {
        return Ok(Op::Property(chunk.to_owned()));
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

    fn prop(name: &str) -> Op {
        Op::Property(name.to_string())
    }

    fn re(pattern: &str) -> Op {
        Op::PropertyRegex(Pattern(Regex::new(pattern).unwrap()))
    }

    #[test]
    fn parse_single_property() {
        assert_eq!(parse(".Tim"), Ok(vec![prop("Tim")]));
    }

    #[test]
    fn parse_nested_properties() {
        assert_eq!(parse(".Tim.age"), Ok(vec![prop("Tim"), prop("age")]));
    }

    #[test]
    fn parse_wildcard() {
        assert_eq!(parse(".*"), Ok(vec![Op::PropertyWildcard]));
    }

    #[test]
    fn parse_wildcard_then_property() {
        assert_eq!(parse(".*.age"), Ok(vec![Op::PropertyWildcard, prop("age")]));
    }

    #[test]
    fn parse_without_leading_dot() {
        assert_eq!(parse("Tim.age"), Ok(vec![prop("Tim"), prop("age")]));
    }

    #[test]
    fn parse_property_with_underscore_and_hyphen() {
        assert_eq!(
            parse(".first_name.last-name"),
            Ok(vec![prop("first_name"), prop("last-name")])
        );
    }

    #[test]
    fn parse_empty_query_yields_no_ops() {
        assert_eq!(parse(""), Ok(vec![]));
        assert_eq!(parse("."), Ok(vec![]));
    }

    #[test]
    fn parse_chunk_wildcard() {
        assert_eq!(parse_chunk("*"), Ok(Op::PropertyWildcard));
    }

    #[test]
    fn parse_chunk_property() {
        assert_eq!(parse_chunk("age"), Ok(prop("age")));
    }

    #[test]
    fn parse_chunk_rejects_invalid() {
        for chunk in ["", "1", "a1", "a b", "**", "age^"] {
            assert!(parse_chunk(chunk).is_err(), "expected error for {chunk:?}");
        }
    }

    #[test]
    fn parse_rejects_invalid_chunk() {
        assert!(parse(".foo.$$.bar").is_err());
        assert!(parse(".foo..bar").is_err());
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
        assert_eq!(parse("./^a/"), Ok(vec![re("^a")]));
        assert_eq!(parse("/^a/.b"), Ok(vec![re("^a"), prop("b")]));
        assert_eq!(parse(".x.//"), Ok(vec![prop("x"), re("")]));
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
            parse(".a.[12].b"),
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
        let p = |s| Pattern(Regex::new(s).unwrap());
        assert_eq!(p("^a+$"), p("^a+$"));
        assert_ne!(p("a"), p("b"));
        // Equivalent regexes with different source are not equal.
        assert_ne!(p("a+"), p("aa*"));
    }

    #[test]
    fn parse_keys() {
        assert_eq!(parse(".^"), Ok(vec![Op::PropertyKeys]));
        assert_eq!(parse("^"), Ok(vec![Op::PropertyKeys]));
        assert_eq!(parse(".a.^"), Ok(vec![prop("a"), Op::PropertyKeys]));
        assert!(parse(".^^").is_err());
    }

    #[test]
    fn parse_rejects_steps_after_keys() {
        for q in [
            ".^.a", ".^.*", ".^.^", ".^.[]", ".^.[0]", ".^.[1:]", ".^./a/", "*.^.a",
        ] {
            assert_eq!(parse(q), Err(ParseError::StepAfterKeys), "query {q:?}");
        }
    }

    #[test]
    fn parse_error_display() {
        assert_eq!(ParseError::InvalidQuery.to_string(), "invalid query");
        assert_eq!(
            ParseError::StepAfterKeys.to_string(),
            "`^` must be the last step"
        );
    }
}
