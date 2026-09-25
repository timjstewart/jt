mod exec;
mod parser;

use exec::{collect, execute};
use parser::{Op, parse};
use serde_json::Value;
use std::error::Error;
use std::io;
use std::process::ExitCode;

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
    let out = eval(&ops, text)?;
    println!("{}", render(&ops, out)?);
    Ok(())
}

/// Pretty-prints query results: a single result as it is, and none or
/// several as a JSON array, except that a query with a slice always gives an array.
fn render(ops: &[Op], out: Vec<Value>) -> serde_json::Result<String> {
    serde_json::to_string_pretty(&collect(ops, out))
}

fn eval(ops: &[Op], text: String) -> Result<Vec<Value>, Box<dyn Error>> {
    let json: Value = serde_json::from_str(&text)?;
    // The parsed document replaces the text, so free the text before running.
    drop(text);
    Ok(execute(ops, vec![json])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::ParseError;

    fn query(q: &str, text: &str) -> String {
        serde_json::to_string(&eval(&parse(q).unwrap(), text.to_owned()).unwrap()).unwrap()
    }

    #[test]
    fn run_rejects_invalid_query() {
        // The query is parsed before the file is read, so a bad query wins.
        let err = run("foo.$$", Some("no/such/file.json")).unwrap_err();
        assert_eq!(
            err.downcast_ref::<ParseError>(),
            Some(&ParseError::InvalidQuery)
        );
    }

    #[test]
    fn run_rejects_missing_file() {
        let err = run("a", Some("no/such/file.json")).unwrap_err();
        assert!(err.downcast_ref::<std::io::Error>().is_some());
    }

    #[test]
    fn run_reads_named_file() {
        let path = std::env::temp_dir().join(format!("jt-test-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"a":1}"#).unwrap();
        let result = run("a", path.to_str());
        std::fs::remove_file(&path).unwrap();
        assert!(result.is_ok());
    }

    #[test]
    fn eval_empty_query_returns_input() {
        assert_eq!(query("", r#"{"a":1}"#), r#"[{"a":1}]"#);
    }

    #[test]
    fn eval_rejects_invalid_json() {
        assert!(eval(&[], "{not json".to_owned()).is_err());
    }

    fn render_query(q: &str, text: &str) -> String {
        let ops = parse(q).unwrap();
        render(&ops, eval(&ops, text.to_owned()).unwrap()).unwrap()
    }

    #[test]
    fn render_empty_query_prints_input_unwrapped() {
        assert_eq!(render_query("", r#"{"a":1}"#), "{\n  \"a\": 1\n}");
        assert_eq!(render_query("", "[1,2]"), "[\n  1,\n  2\n]");
        assert_eq!(render_query("", "7"), "7");
    }

    #[test]
    fn render_single_result_unwrapped() {
        assert_eq!(render_query("a", r#"{"a":1}"#), "1");
        assert_eq!(render_query("a", r#"{"a":{"b":2}}"#), "{\n  \"b\": 2\n}");
        assert_eq!(render_query("a", r#"{"a":[1]}"#), "[\n  1\n]");
    }

    #[test]
    fn render_slice_as_array_even_with_one_result() {
        assert_eq!(render_query("*", r#"{"a":1}"#), "1");
        assert_eq!(render_query("[0:1]", "[1,2]"), "[\n  1\n]");
    }

    #[test]
    fn render_none_or_several_results_as_array() {
        assert_eq!(render_query("*", "{}"), "[]");
        assert_eq!(render_query("*", r#"{"a":1,"b":2}"#), "[\n  1,\n  2\n]");
    }
}
