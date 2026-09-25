use regex::Regex;
use serde_json::{Value, from_str};
use std::error::Error;
use std::fmt;
use std::fs::read_to_string;
use std::process::ExitCode;
use std::sync::OnceLock;

static PROPERTY_REGEX: OnceLock<Regex> = OnceLock::new();

fn get_property_regex() -> &'static Regex {
    PROPERTY_REGEX.get_or_init(|| Regex::new("^[a-zA-Z_-]+$").unwrap())
}

#[derive(Debug, PartialEq)]
enum ParseError {
    Unknown,
    NotAnObject,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            _ => write!(f, "unknown parse error"),
        }
    }
}

impl Error for ParseError {}

#[derive(Debug, PartialEq)]
enum Op {
    Property(String),
    PropertyWildCard,
}

fn parse(query: &str) -> Result<Vec<Op>, ParseError> {
    let mut result = Vec::<Op>::new();
    let chunks = query.strip_prefix('.').unwrap_or(query).split('.');

    for chunk in chunks {
        println!("CHUNK: {:?}", chunk);
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
    };
    Err(ParseError::Unknown)
}

fn main() -> ExitCode {
    match run() {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    match parse(".*") {
        Ok(ops) => {
            let text = read_to_string("input.json")?;
            let json: Value = from_str(&text)?;
            execute(&ops, &json)?;
            Ok(())
        }
        Err(err) => {
            println!("Failed: {:?}", err);
            Err(Box::new(ParseError::Unknown))
        }
    }
}

fn execute(ops: &Vec<Op>, json: &Value) -> Result<Value, ParseError> {
    let result: Option<serde_json::Value> = None;

    for op in ops {
        match op {
            Op::Property(name) => match json {
                Value::Object(obj) => {
                    if obj.contains_key(name) {
                    } else {
                    }
                }
                _ => return Err(ParseError::NotAnObject),
            },
            Op::PropertyWildCard => {}
        }
    }

    match result {
        Some(json) => Ok(json),
        None => Err(ParseError::Unknown),
    }
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
