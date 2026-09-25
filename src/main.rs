use regex::Regex;
use serde_json::{Value, from_str, to_string};
use std::error::Error;
use std::fmt;
use std::fs::read_to_string;
use std::process::ExitCode;
use std::sync::OnceLock;

use crate::ParseError::NotAnObject;

static PROPERTY_REGEX: OnceLock<Regex> = OnceLock::new();

fn get_property_regex() -> &'static Regex {
    PROPERTY_REGEX.get_or_init(|| Regex::new("^[a-zA-Z_-]+$").unwrap())
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
}

fn parse(query: &str) -> Result<Vec<Op>, ParseError> {
    let mut result = Vec::<Op>::new();
    let chunks = query.strip_prefix('.').unwrap_or(query).split('.');

    for chunk in chunks {
        match parse_chunk(chunk) {
            Ok(ops) => result.extend(ops),
            Err(err) => println!("Error: {:?}", err),
        }
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
    };
    Err(ParseError::UnknownError)
}

fn main() -> ExitCode {
    match run() {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    match parse("[].*") {
        Ok(ops) => {
            let text = read_to_string("ainput.json")?;
            let json: Value = from_str(&text)?;
            let out = execute(&ops, vec![json])?;
            let result = to_string(&out)?;
            println!("{}", result);
            Ok(())
        }
        Err(err) => {
            println!("Failed: {:?}", err);
            Err(Box::new(ParseError::UnknownError))
        }
    }
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
                next_input.extend(array.iter().cloned())
            },
            _ => todo!()
        }
        if let Value::Object(obj) = node {}
    }
    execute(rest, next_input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prop(name: &str) -> Op {
        Op::Property(name.to_string())
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
}
