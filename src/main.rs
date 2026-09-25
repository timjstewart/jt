use regex::Regex;
use serde_json::Value;
use std::error::Error;
use std::fmt;
use std::io;
use std::ops::Range;
use std::process::ExitCode;
use std::sync::LazyLock;

static PROPERTY_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new("^[a-zA-Z_-]+$").unwrap());
static ARRAY_INDEX_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[([0-9]+)\]$").unwrap());
static ARRAY_SLICE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\[(-?[0-9]+)?:(-?[0-9]+)?\]$").unwrap());

/// The query text could not be parsed.
#[derive(Debug, PartialEq, Eq)]
enum ParseError {
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

/// A parsed query could not be applied to the input.
#[derive(Debug, PartialEq, Eq)]
enum ExecError {
    NotAnObject,
    NotAnArray,
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::NotAnObject => f.write_str("not an object"),
            Self::NotAnArray => f.write_str("not an array"),
        }
    }
}

impl Error for ExecError {}

/// A compiled regex that compares equal to another with the same pattern.
#[derive(Debug)]
struct Pattern(Regex);

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

#[derive(Debug, PartialEq)]
enum Op {
    // Object operations
    Property(String),
    PropertyWildcard,
    PropertyRegex(Pattern),
    // Array operations
    Array,
    ArrayIndex(usize),
    ArraySlice {
        start: Option<isize>,
        stop: Option<isize>,
    },
}

fn parse(query: &str) -> Result<Vec<Op>, ParseError> {
    let query = query.strip_prefix('.').unwrap_or(query);
    if query.is_empty() {
        return Ok(vec![]);
    }
    split_chunks(query)?.into_iter().map(parse_chunk).collect()
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

/// Range selected by a Python-style `[start:stop]` on a sequence of length `len`.
fn slice_range(len: usize, start: Option<isize>, stop: Option<isize>) -> Range<usize> {
    // A Vec never holds more than isize::MAX elements, so these casts are lossless.
    let n = len.cast_signed();
    // Negative bounds count from the end; out-of-range bounds clamp, as in Python.
    let clamp = |i: isize| (if i < 0 { i + n } else { i }).clamp(0, n).cast_unsigned();
    let start = start.map_or(0, clamp);
    let stop = stop.map_or(len, clamp).max(start);
    start..stop
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let (query, path) = match args.as_slice() {
        [_, query] => (query, None),
        [_, query, path] => (query, Some(path.as_str())),
        _ => {
            eprintln!("usage: jt <query> [file]");
            return ExitCode::FAILURE;
        }
    };
    match run(query, path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jt: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Runs `query` against the JSON in the file at `path`, or on stdin when there is no path.
fn run(query: &str, path: Option<&str>) -> Result<(), Box<dyn Error>> {
    let ops = parse(query)?;
    let text = match path {
        Some(path) => std::fs::read_to_string(path)?,
        None => io::read_to_string(io::stdin())?,
    };
    let out = eval(&ops, &text)?;
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

fn eval(ops: &[Op], text: &str) -> Result<Vec<Value>, Box<dyn Error>> {
    let json: Value = serde_json::from_str(text)?;
    Ok(execute(ops, vec![json])?)
}

/// Applies each op in turn to every value produced by the previous one.
fn execute(ops: &[Op], input: Vec<Value>) -> Result<Vec<Value>, ExecError> {
    ops.iter().try_fold(input, |nodes, op| {
        let mut out = vec![];
        for node in &nodes {
            apply(op, node, &mut out)?;
        }
        Ok(out)
    })
}

/// Applies a single op to a single value, appending the results to `out`.
fn apply(op: &Op, node: &Value, out: &mut Vec<Value>) -> Result<(), ExecError> {
    match (op, node) {
        (Op::Property(name), Value::Object(obj)) => {
            out.push(obj.get(name).cloned().unwrap_or_default());
        }
        (Op::PropertyWildcard, Value::Object(obj)) => out.extend(obj.values().cloned()),
        (Op::PropertyRegex(Pattern(regex)), Value::Object(obj)) => out.extend(
            obj.iter()
                .filter(|(key, _)| regex.is_match(key))
                .map(|(_, value)| value.clone()),
        ),
        (Op::Array, Value::Array(array)) => out.extend(array.iter().cloned()),
        (Op::ArrayIndex(n), Value::Array(array)) => {
            out.push(array.get(*n).cloned().unwrap_or_default());
        }
        (Op::ArraySlice { start, stop }, Value::Array(array)) => {
            let range = slice_range(array.len(), *start, *stop);
            out.push(Value::Array(array[range].to_vec()));
        }
        // Looking up a single element of null yields null, as in jq.
        (Op::Property(_) | Op::ArrayIndex(_) | Op::ArraySlice { .. }, Value::Null) => {
            out.push(Value::Null);
        }
        (Op::Property(_) | Op::PropertyWildcard | Op::PropertyRegex(_), _) => {
            return Err(ExecError::NotAnObject);
        }
        (Op::Array | Op::ArrayIndex(_) | Op::ArraySlice { .. }, _) => {
            return Err(ExecError::NotAnArray);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn prop(name: &str) -> Op {
        Op::Property(name.to_string())
    }

    fn query(q: &str, text: &str) -> String {
        serde_json::to_string(&eval(&parse(q).unwrap(), text).unwrap()).unwrap()
    }

    #[test]
    fn run_rejects_invalid_query() {
        // The query is parsed before the file is read, so a bad query wins.
        let err = run(".foo.$$", Some("no/such/file.json")).unwrap_err();
        assert_eq!(
            err.downcast_ref::<ParseError>(),
            Some(&ParseError::InvalidQuery)
        );
    }

    #[test]
    fn run_rejects_missing_file() {
        let err = run(".a", Some("no/such/file.json")).unwrap_err();
        assert!(err.downcast_ref::<std::io::Error>().is_some());
    }

    #[test]
    fn run_reads_named_file() {
        let path = std::env::temp_dir().join(format!("jt-test-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"a":1}"#).unwrap();
        let result = run(".a", path.to_str());
        std::fs::remove_file(&path).unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn eval_empty_query_returns_input() {
        assert_eq!(query(".", r#"{"a":1}"#), r#"[{"a":1}]"#);
    }

    #[test]
    fn eval_property() {
        assert_eq!(query(".a", r#"{"a":[1,2,3]}"#), "[[1,2,3]]");
    }

    #[test]
    fn eval_nested_property() {
        assert_eq!(query(".a.name", r#"{"a":{"name":"foo"}}"#), r#"["foo"]"#);
    }

    #[test]
    fn eval_missing_property_is_null() {
        assert_eq!(query(".nope.x", r#"{"a":1}"#), "[null]");
    }

    #[test]
    fn eval_wildcard_then_property() {
        let text = r#"{"b":{"name":"bar"},"a":{"name":"foo"}}"#;
        assert_eq!(query(".*.name", text), r#"["bar","foo"]"#);
    }

    #[test]
    fn eval_array_then_wildcard() {
        let text = r#"[{"a":{"name":"foo"}},{"b":{"name":"bar"}}]"#;
        assert_eq!(query("[].*", text), r#"[{"name":"foo"},{"name":"bar"}]"#);
    }

    #[test]
    fn eval_array_index_then_wildcard() {
        let text = r#"[{"a":{"name":"foo"}},{"b":{"name":"bar"}}]"#;
        assert_eq!(query("[1].*", text), r#"[{"name":"bar"}]"#);
    }

    #[test]
    fn eval_property_on_scalar_errors() {
        assert!(eval(&parse(".a.x").unwrap(), r#"{"a":1}"#).is_err());
    }

    #[test]
    fn eval_array_op_on_object_errors() {
        assert!(eval(&parse("[]").unwrap(), r#"{"a":1}"#).is_err());
    }

    #[test]
    fn eval_rejects_invalid_json() {
        assert!(eval(&[], "{not json").is_err());
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
    fn execute_property_on_null_yields_null() {
        let input = json!({"a": 1});
        assert_eq!(
            execute(&[prop("nope"), prop("x")], vec![input]),
            Ok(vec![Value::Null])
        );
    }

    #[test]
    fn execute_property_on_scalar_errors() {
        let input = json!({"a": 1});
        assert_eq!(
            execute(&[prop("a"), prop("x")], vec![input]),
            Err(ExecError::NotAnObject)
        );
    }

    #[test]
    fn execute_wildcard_preserves_document_order() {
        let input: Value = serde_json::from_str(r#"{"b": 1, "a": 2}"#).unwrap();
        assert_eq!(
            execute(&[Op::PropertyWildcard], vec![input]),
            Ok(vec![Value::from(1), Value::from(2)])
        );
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
    fn eval_slice_matches_python() {
        // Expected values produced by Python on [0, 1, 2, 3, 4].
        let cases = [
            ("[1:3]", "[[1,2]]"),
            ("[-2:]", "[[3,4]]"),
            ("[:-1]", "[[0,1,2,3]]"),
            ("[10:20]", "[[]]"),
            ("[-10:2]", "[[0,1]]"),
            ("[3:1]", "[[]]"),
            ("[-1:-3]", "[[]]"),
            ("[:]", "[[0,1,2,3,4]]"),
            ("[:10]", "[[0,1,2,3,4]]"),
        ];
        for (q, expected) in cases {
            assert_eq!(query(q, "[0,1,2,3,4]"), expected, "query {q:?}");
        }
    }

    #[test]
    fn eval_slice_then_iterate() {
        let text = r#"[{"n":"a"},{"n":"b"},{"n":"c"}]"#;
        assert_eq!(query("[1:].[].n", text), r#"["b","c"]"#);
    }

    #[test]
    fn eval_slice_on_null_is_null() {
        assert_eq!(query(".nope.[1:2]", r#"{"a":1}"#), "[null]");
    }

    #[test]
    fn eval_slice_on_object_errors() {
        assert!(eval(&parse("[1:2]").unwrap(), r#"{"a":1}"#).is_err());
    }

    fn re(pattern: &str) -> Op {
        Op::PropertyRegex(Pattern(Regex::new(pattern).unwrap()))
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
    fn eval_regex_collects_matching_properties_in_order() {
        let text = r#"{"name":"x","age":1,"nickname":"y","id":2}"#;
        assert_eq!(query("/name/", text), r#"["x","y"]"#);
        assert_eq!(query("/^name$/", text), r#"["x"]"#);
        assert_eq!(query("/^(age|id)$/", text), "[1,2]");
        assert_eq!(query("/zzz/", text), "[]");
    }

    #[test]
    fn eval_regex_then_property() {
        let text = r#"{"user_a":{"n":1},"user_b":{"n":2},"admin":{"n":3}}"#;
        assert_eq!(query("/^user_/.n", text), "[1,2]");
    }

    #[test]
    fn eval_regex_on_non_object_errors() {
        for text in ["[1,2]", "null", "1"] {
            assert!(
                eval(&parse("/a/").unwrap(), text).is_err(),
                "expected error for {text}"
            );
        }
    }

    fn exec(q: &str, input: Value) -> Result<Vec<Value>, ExecError> {
        execute(&parse(q).unwrap(), vec![input])
    }

    // split_chunks

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

    // parse: array index and iteration

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

    // slice_range

    #[test]
    fn slice_range_on_empty_sequence() {
        assert_eq!(slice_range(0, None, None), 0..0);
        assert_eq!(slice_range(0, Some(-1), Some(5)), 0..0);
    }

    #[test]
    fn slice_range_never_runs_backwards() {
        assert_eq!(slice_range(5, Some(4), Some(1)), 4..4);
        assert_eq!(slice_range(5, Some(-1), Some(0)), 4..4);
    }

    // Pattern

    #[test]
    fn pattern_equality_compares_source() {
        let p = |s| Pattern(Regex::new(s).unwrap());
        assert_eq!(p("^a+$"), p("^a+$"));
        assert_ne!(p("a"), p("b"));
        // Equivalent regexes with different source are not equal.
        assert_ne!(p("a+"), p("aa*"));
    }

    // execute: arrays

    #[test]
    fn execute_array_iterates_elements() {
        assert_eq!(
            exec("[]", json!([1, "x", null])),
            Ok(vec![json!(1), json!("x"), Value::Null])
        );
        assert_eq!(exec("[]", json!([])), Ok(vec![]));
    }

    #[test]
    fn execute_array_index() {
        assert_eq!(exec("[1]", json!([10, 20, 30])), Ok(vec![json!(20)]));
        assert_eq!(exec("[3]", json!([10, 20, 30])), Ok(vec![Value::Null]));
        assert_eq!(exec("[0]", json!([])), Ok(vec![Value::Null]));
    }

    #[test]
    fn execute_array_index_on_null_is_null() {
        assert_eq!(exec(".nope.[0]", json!({})), Ok(vec![Value::Null]));
    }

    #[test]
    fn execute_slice_of_empty_array() {
        assert_eq!(exec("[1:3]", json!([])), Ok(vec![json!([])]));
    }

    // execute: fan-out over several nodes

    #[test]
    fn execute_applies_op_to_every_node() {
        let input = json!([{"n": 1}, {"m": 2}, {"n": 3}]);
        assert_eq!(
            exec("[].n", input),
            Ok(vec![json!(1), Value::Null, json!(3)])
        );
    }

    #[test]
    fn execute_empty_ops_returns_input() {
        assert_eq!(
            execute(&[], vec![json!(1), json!(2)]),
            Ok(vec![json!(1), json!(2)])
        );
    }

    #[test]
    fn execute_error_in_any_node_fails_whole_query() {
        assert_eq!(
            exec("[].n", json!([{"n": 1}, 2])),
            Err(ExecError::NotAnObject)
        );
    }

    // execute: error variants

    #[test]
    fn execute_array_ops_on_object_are_not_an_array() {
        for q in ["[]", "[0]", "[1:2]"] {
            assert_eq!(
                exec(q, json!({"a": 1})),
                Err(ExecError::NotAnArray),
                "query {q:?}"
            );
        }
    }

    #[test]
    fn execute_array_ops_on_scalar_are_not_an_array() {
        for q in ["[]", "[0]", "[1:2]"] {
            for input in [json!(1), json!("s"), json!(true)] {
                assert_eq!(
                    exec(q, input.clone()),
                    Err(ExecError::NotAnArray),
                    "query {q:?} on {input}"
                );
            }
        }
    }

    #[test]
    fn execute_object_ops_on_scalar_are_not_an_object() {
        for q in [".a", "*", "/a/"] {
            for input in [json!(1), json!("s"), json!(false)] {
                assert_eq!(
                    exec(q, input.clone()),
                    Err(ExecError::NotAnObject),
                    "query {q:?} on {input}"
                );
            }
        }
    }

    #[test]
    fn execute_iteration_ops_on_null_error() {
        assert_eq!(exec(".x.[]", json!({})), Err(ExecError::NotAnArray));
        assert_eq!(exec(".x.*", json!({})), Err(ExecError::NotAnObject));
        assert_eq!(exec(".x./a/", json!({})), Err(ExecError::NotAnObject));
    }

    #[test]
    fn execute_object_ops_on_array_are_not_an_object() {
        for q in [".a", "*", "/a/"] {
            assert_eq!(
                exec(q, json!([{"a": 1}])),
                Err(ExecError::NotAnObject),
                "query {q:?}"
            );
        }
    }

    #[test]
    fn error_display() {
        assert_eq!(ParseError::InvalidQuery.to_string(), "invalid query");
        assert_eq!(ExecError::NotAnObject.to_string(), "not an object");
        assert_eq!(ExecError::NotAnArray.to_string(), "not an array");
    }
}
