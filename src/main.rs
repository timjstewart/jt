mod exec;
mod parser;

use clap::Parser;
use colored_json::ColorMode;
use exec::{collect, execute};
use parser::{Op, parse};
use serde_json::Value;
use std::error::Error;
use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;

/// Pulls values out of JSON, in the spirit of jq.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Color the output even when it is piped or redirected
    #[arg(short = 'C', long)]
    color: bool,
    /// Keep object keys whose values are null
    #[arg(long)]
    keep_nulls: bool,
    /// The query to run; '' prints the input unchanged
    query: String,
    /// The JSON file to read; stdin if omitted
    file: Option<PathBuf>,
}

fn main() -> ExitCode {
    match run(&Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("jt: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Runs the query against the JSON in the file, or on stdin when there is no file.
fn run(args: &Args) -> Result<(), Box<dyn Error>> {
    let ops = parse(&args.query)?;
    let text = match &args.file {
        Some(path) => std::fs::read_to_string(path)?,
        None => io::read_to_string(io::stdin())?,
    };
    let out = eval(&ops, text)?;
    // A single result prints as it is, and none or several as a JSON array,
    // except that a query with a slice always prints an array.
    let mut value = collect(&ops, out);
    if !args.keep_nulls {
        remove_null_keys(&mut value);
    }
    // Color the output unless it is redirected and color isn't forced.
    let color = if args.color || io::stdout().is_terminal() {
        ColorMode::On
    } else {
        ColorMode::Off
    };
    println!("{}", colored_json::to_colored_json(&value, color)?);
    Ok(())
}

/// Removes every object key whose value is `null`, at any depth. Nulls in
/// arrays, and a `null` result on its own, are kept.
fn remove_null_keys(value: &mut Value) {
    match value {
        Value::Object(obj) => {
            obj.retain(|_, v| !v.is_null());
            obj.values_mut().for_each(remove_null_keys);
        }
        Value::Array(array) => array.iter_mut().for_each(remove_null_keys),
        _ => {}
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

    fn args(query: &str, file: &str) -> Args {
        Args {
            color: false,
            keep_nulls: false,
            query: query.to_owned(),
            file: Some(file.into()),
        }
    }

    fn query(q: &str, text: &str) -> String {
        serde_json::to_string(&eval(&parse(q).unwrap(), text.to_owned()).unwrap()).unwrap()
    }

    #[test]
    fn run_rejects_invalid_query() {
        // The query is parsed before the file is read, so a bad query wins.
        let err = run(&args("foo.$$", "no/such/file.json")).unwrap_err();
        assert_eq!(
            err.downcast_ref::<ParseError>(),
            Some(&ParseError::InvalidQuery)
        );
    }

    #[test]
    fn run_rejects_missing_file() {
        let err = run(&args("a", "no/such/file.json")).unwrap_err();
        assert!(err.downcast_ref::<std::io::Error>().is_some());
    }

    #[test]
    fn run_reads_named_file() {
        let path = std::env::temp_dir().join(format!("jt-test-{}.json", std::process::id()));
        std::fs::write(&path, r#"{"a":1}"#).unwrap();
        let result = run(&args("a", path.to_str().unwrap()));
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

    /// The value `run` prints for `q`, before null keys are removed, as compact JSON.
    fn output(q: &str, text: &str) -> String {
        let ops = parse(q).unwrap();
        collect(&ops, eval(&ops, text.to_owned()).unwrap()).to_string()
    }

    #[test]
    fn output_empty_query_is_input_unwrapped() {
        assert_eq!(output("", r#"{"a":1}"#), r#"{"a":1}"#);
        assert_eq!(output("", "[1,2]"), "[1,2]");
        assert_eq!(output("", "7"), "7");
    }

    #[test]
    fn output_single_result_unwrapped() {
        assert_eq!(output("a", r#"{"a":1}"#), "1");
        assert_eq!(output("a", r#"{"a":{"b":2}}"#), r#"{"b":2}"#);
        assert_eq!(output("a", r#"{"a":[1]}"#), "[1]");
    }

    #[test]
    fn output_slice_as_array_even_with_one_result() {
        assert_eq!(output("*", r#"{"a":1}"#), "1");
        assert_eq!(output("[0:1]", "[1,2]"), "[1]");
    }

    #[test]
    fn output_none_or_several_results_as_array() {
        assert_eq!(output("*", "{}"), "[]");
        assert_eq!(output("*", r#"{"a":1,"b":2}"#), "[1,2]");
    }

    #[test]
    fn remove_null_keys_at_any_depth() {
        let mut value = serde_json::json!({"a":null,"b":{"c":null,"d":1},"e":[null,{"f":null}]});
        remove_null_keys(&mut value);
        assert_eq!(value.to_string(), r#"{"b":{"d":1},"e":[null,{}]}"#);
    }

    #[test]
    fn remove_null_keys_keeps_a_null_value() {
        let mut value = Value::Null;
        remove_null_keys(&mut value);
        assert_eq!(value, Value::Null);
    }

    #[test]
    fn args_parse_flags_and_positionals() {
        let a = Args::try_parse_from(["jt", "-C", "--keep-nulls", "a.b", "f.json"]).unwrap();
        assert!(a.color && a.keep_nulls);
        assert_eq!((a.query.as_str(), a.file), ("a.b", Some("f.json".into())));
        let a = Args::try_parse_from(["jt", ""]).unwrap();
        assert!(!a.color && !a.keep_nulls && a.query.is_empty() && a.file.is_none());
        assert!(Args::try_parse_from(["jt"]).is_err());
        assert!(Args::try_parse_from(["jt", "a", "f", "g"]).is_err());
    }
}
