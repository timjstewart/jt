use regex::Regex;
use serde_json::{Value, from_str, to_string};
use std::error::Error;
use std::fmt;
use std::fs::read_to_string;
use std::ops::Range;
use std::process::ExitCode;
use std::sync::OnceLock;

static PROPERTY_REGEX: OnceLock<Regex> = OnceLock::new();
static ARRAY_INDEX_REGEX: OnceLock<Regex> = OnceLock::new();
static ARRAY_SLICE_REGEX: OnceLock<Regex> = OnceLock::new();

fn get_property_regex() -> &'static Regex {
    PROPERTY_REGEX.get_or_init(|| Regex::new("^[a-zA-Z_-]+$").unwrap())
}

fn get_array_index_regex() -> &'static Regex {
    ARRAY_INDEX_REGEX.get_or_init(|| Regex::new(r"^\[([0-9]+)\]$").unwrap())
}

fn get_array_slice_regex() -> &'static Regex {
    ARRAY_SLICE_REGEX
        .get_or_init(|| Regex::new(r"^\[(-?[0-9]+)?:(-?[0-9]+)?\]$").unwrap())
}

#[derive(Debug, PartialEq)]
enum ParseError {
    UnknownError,
    NotAnObject,
    NotAnArray,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ParseError::UnknownError => write!(f, "unknown parse error"),
            ParseError::NotAnObject => write!(f, "not an object"),
            ParseError::NotAnArray => write!(f, "not an array"),
        }
    }
}

impl Error for ParseError {}

#[derive(Debug, PartialEq)]
enum Op {
    Property(String),
    PropertyWildCard,
    Array,
    ArrayIndex(usize),
    ArraySlice {
        start: Option<isize>,
        stop: Option<isize>,
    },
}

fn parse(query: &str) -> Result<Vec<Op>, ParseError> {
    let mut result = Vec::<Op>::new();
    let query = query.strip_prefix('.').unwrap_or(query);
    if query.is_empty() {
        return Ok(result);
    }

    for chunk in query.split('.') {
        result.extend(parse_chunk(chunk)?);
    }
    Ok(result)
}

fn parse_chunk(chunk: &str) -> Result<Vec<Op>, ParseError> {
    if chunk == "*" {
        return Ok(vec![Op::PropertyWildCard]);
    } else if get_property_regex().is_match(chunk) {
        return Ok(vec![Op::Property(chunk.to_string())]);
    } else if chunk == "[]" {
        return Ok(vec![Op::Array]);
    } else if let Some(caps) = get_array_index_regex().captures(chunk) {
        let n = caps[1].parse().map_err(|_| ParseError::UnknownError)?;
        return Ok(vec![Op::ArrayIndex(n)]);
    } else if let Some(caps) = get_array_slice_regex().captures(chunk) {
        let part = |i: usize| -> Result<Option<isize>, ParseError> {
            caps.get(i)
                .map(|m| m.as_str().parse().map_err(|_| ParseError::UnknownError))
                .transpose()
        };
        return Ok(vec![Op::ArraySlice {
            start: part(1)?,
            stop: part(2)?,
        }]);
    };
    Err(ParseError::UnknownError)
}

/// Range selected by a Python-style `[start:stop]` on a sequence of length `len`.
fn slice_range(len: usize, start: Option<isize>, stop: Option<isize>) -> Range<usize> {
    let n = len as isize;
    // Negative bounds count from the end; out-of-range bounds clamp, as in Python.
    let clamp = |i: isize| (if i < 0 { i + n } else { i }).clamp(0, n) as usize;
    let start = start.map_or(0, clamp);
    let stop = stop.map_or(len, clamp).max(start);
    start..stop
}

fn main() -> ExitCode {
    match run("[0].*") {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn run(query: &str) -> Result<(), Box<dyn Error>> {
    match parse(query) {
        Ok(ops) => {
            let text = read_to_string("ainput.json")?;
            println!("{}", eval(&ops, &text)?);
            Ok(())
        }
        Err(err) => {
            println!("Failed: {:?}", err);
            Err(Box::new(ParseError::UnknownError))
        }
    }
}

fn eval(ops: &[Op], text: &str) -> Result<String, Box<dyn Error>> {
    let json: Value = from_str(text)?;
    let out = execute(ops, vec![json])?;
    Ok(to_string(&out)?)
}

fn execute(ops: &[Op], input: Vec<Value>) -> Result<Vec<Value>, ParseError> {
    let Some((op, rest)) = ops.split_first() else {
        return Ok(input);
    };
    let mut next_input = vec![];
    for node in &input {
        match node {
            Value::Object(obj) => match op {
                Op::Property(name) => {
                    next_input.push(obj.get(name).cloned().unwrap_or(Value::Null))
                }
                Op::PropertyWildCard => next_input.extend(obj.values().cloned()),
                _ => return Err(ParseError::NotAnArray),
            },
            Value::Array(array) => {
                match op {
                    Op::Array => next_input.extend(array.iter().cloned()),
                    Op::ArrayIndex(n) => next_input.push(array.get(*n).cloned().unwrap_or(Value::Null)),
                    Op::ArraySlice { start, stop } => {
                        let range = slice_range(array.len(), *start, *stop);
                        next_input.push(Value::Array(array[range].to_vec()))
                    }
                    _ => return Err(ParseError::UnknownError),

                }
            }
            Value::Null => match op {
                Op::Property(_) | Op::ArrayIndex(_) | Op::ArraySlice { .. } => {
                    next_input.push(Value::Null)
                }
                Op::PropertyWildCard => return Err(ParseError::NotAnObject),
                Op::Array => return Err(ParseError::NotAnArray),
            },
            _ => match op {
                Op::Property(_) | Op::PropertyWildCard => return Err(ParseError::NotAnObject),
                Op::Array | Op::ArrayIndex(_) | Op::ArraySlice { .. } => {
                    return Err(ParseError::NotAnArray);
                }
            },
        }
    }
    execute(rest, next_input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prop(name: &str) -> Op {
        Op::Property(name.to_string())
    }

    fn query(q: &str, text: &str) -> String {
        eval(&parse(q).unwrap(), text).unwrap()
    }

    #[test]
    fn run_rejects_invalid_query() {
        assert!(run(".foo.$$").is_err());
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
        assert_eq!(parse(".*"), Ok(vec![Op::PropertyWildCard]));
    }

    #[test]
    fn parse_wildcard_then_property() {
        assert_eq!(parse(".*.age"), Ok(vec![Op::PropertyWildCard, prop("age")]));
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
        assert_eq!(parse_chunk("*").unwrap(), vec![Op::PropertyWildCard]);
    }

    #[test]
    fn parse_chunk_property() {
        assert_eq!(parse_chunk("age").unwrap(), vec![prop("age")]);
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
        let input = serde_json::json!({"a": 1});
        assert_eq!(
            execute(&[prop("nope"), prop("x")], vec![input]),
            Ok(vec![Value::Null])
        );
    }

    #[test]
    fn execute_property_on_scalar_errors() {
        let input = serde_json::json!({"a": 1});
        assert_eq!(
            execute(&[prop("a"), prop("x")], vec![input]),
            Err(ParseError::NotAnObject)
        );
    }

    #[test]
    fn execute_wildcard_preserves_document_order() {
        let input: Value = from_str(r#"{"b": 1, "a": 2}"#).unwrap();
        assert_eq!(
            execute(&[Op::PropertyWildCard], vec![input]),
            Ok(vec![Value::from(1), Value::from(2)])
        );
    }

    #[test]
    fn parse_slice() {
        assert_eq!(
            parse("[1:-2]"),
            Ok(vec![Op::ArraySlice { start: Some(1), stop: Some(-2) }])
        );
        assert_eq!(
            parse("[:]"),
            Ok(vec![Op::ArraySlice { start: None, stop: None }])
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
}
