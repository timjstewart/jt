mod exec;
mod parser;

use exec::execute;
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
    println!("{}", render(&ops, &out)?);
    Ok(())
}

/// Pretty-prints query results as a JSON array, except that a query with no
/// steps prints the input unchanged.
fn render(ops: &[Op], out: &[Value]) -> serde_json::Result<String> {
    match (ops, out) {
        ([], [input]) => serde_json::to_string_pretty(input),
        _ => serde_json::to_string_pretty(out),
    }
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
    fn eval_rejects_invalid_json() {
        assert!(eval(&[], "{not json".to_owned()).is_err());
    }

    fn render_query(q: &str, text: &str) -> String {
        let ops = parse(q).unwrap();
        render(&ops, &eval(&ops, text.to_owned()).unwrap()).unwrap()
    }

    #[test]
    fn render_empty_query_prints_input_unwrapped() {
        for q in ["", "."] {
            assert_eq!(
                render_query(q, r#"{"a":1}"#),
                "{\n  \"a\": 1\n}",
                "query {q:?}"
            );
            assert_eq!(render_query(q, "[1,2]"), "[\n  1,\n  2\n]", "query {q:?}");
            assert_eq!(render_query(q, "7"), "7", "query {q:?}");
        }
    }

    #[test]
    fn render_query_with_steps_is_always_an_array() {
        assert_eq!(render_query(".a", r#"{"a":1}"#), "[\n  1\n]");
        assert_eq!(
            render_query(".a", r#"{"a":{"b":2}}"#),
            "[\n  {\n    \"b\": 2\n  }\n]"
        );
        assert_eq!(render_query("*", "{}"), "[]");
    }
}
