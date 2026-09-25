//! Runs parsed queries against JSON values.

use crate::parser::Op;
use serde_json::{Map, Value};
use std::error::Error;
use std::fmt;
use std::ops::Range;

/// A parsed query could not be applied to the input.
#[derive(Debug, PartialEq, Eq)]
pub enum ExecError {
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

/// Applies each op in turn to every value produced by the previous one.
pub fn execute(ops: &[Op], input: Vec<Value>) -> Result<Vec<Value>, ExecError> {
    ops.iter().try_fold(input, |nodes, op| {
        let mut out = vec![];
        for node in nodes {
            apply(op, node, &mut out)?;
        }
        Ok(out)
    })
}

/// Applies a single op to a single value, appending the results to `out`.
///
/// Takes `node` by value so that selected parts are moved out rather than
/// cloned; each value is used by exactly one step, and whatever isn't selected
/// is dropped here.
fn apply(op: &Op, node: Value, out: &mut Vec<Value>) -> Result<(), ExecError> {
    match (op, node) {
        (Op::Property(name), Value::Object(mut obj)) => {
            out.push(obj.remove(name).unwrap_or_default());
        }
        (Op::PropertyPick(names), Value::Object(obj)) => out.push(pick(names, obj)),
        (Op::PropertyWildcard, Value::Object(obj)) => out.extend(obj.into_values()),
        (Op::PropertyKeys, Value::Object(obj)) => {
            out.extend(obj.into_iter().map(|(key, _)| Value::String(key)));
        }
        (Op::PropertyMap(ops), Value::Object(obj)) => {
            let mapped = obj
                .into_iter()
                .map(|(key, value)| Ok((key, single_or_array(execute(ops, vec![value])?))))
                .collect::<Result<_, _>>()?;
            out.push(Value::Object(mapped));
        }
        (Op::PropertyRegex(pattern), Value::Object(obj)) => out.extend(
            obj.into_iter()
                .filter(|(key, _)| pattern.is_match(key))
                .map(|(_, value)| value),
        ),
        (Op::Array, Value::Array(array)) => out.extend(array),
        (Op::ArrayIndex(n), Value::Array(array)) => {
            out.push(array.into_iter().nth(*n).unwrap_or_default());
        }
        (Op::ArraySlice { start, stop }, Value::Array(mut array)) => {
            // Trim the array in place, reusing its allocation.
            let range = slice_range(array.len(), *start, *stop);
            array.truncate(range.end);
            array.drain(..range.start);
            out.push(Value::Array(array));
        }
        // Looking up a single element of null yields null, as in jq.
        (Op::Property(_) | Op::ArrayIndex(_) | Op::ArraySlice { .. }, Value::Null) => {
            out.push(Value::Null);
        }
        // Every property of null is null, so picking from null gives all nulls, as in jq.
        (Op::PropertyPick(names), Value::Null) => out.push(pick(names, Map::new())),
        (
            Op::Property(_)
            | Op::PropertyPick(_)
            | Op::PropertyWildcard
            | Op::PropertyKeys
            | Op::PropertyMap(_)
            | Op::PropertyRegex(_),
            _,
        ) => {
            return Err(ExecError::NotAnObject);
        }
        (Op::Array | Op::ArrayIndex(_) | Op::ArraySlice { .. }, _) => {
            return Err(ExecError::NotAnArray);
        }
    }
    Ok(())
}

/// The only value in `values`, or else all of them as an array.
fn single_or_array(mut values: Vec<Value>) -> Value {
    match values.pop() {
        Some(value) if values.is_empty() => value,
        Some(value) => {
            values.push(value);
            Value::Array(values)
        }
        None => Value::Array(values),
    }
}

/// Builds an object holding only the properties `names` of `obj`, in that
/// order, with `null` for any that are missing.
fn pick(names: &[String], mut obj: Map<String, Value>) -> Value {
    names
        .iter()
        .map(|name| {
            obj.remove_entry(name)
                .unwrap_or_else(|| (name.clone(), Value::Null))
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;
    use serde_json::json;

    fn prop(name: &str) -> Op {
        Op::Property(name.to_string())
    }

    fn exec(q: &str, input: Value) -> Result<Vec<Value>, ExecError> {
        execute(&parse(q).unwrap(), vec![input])
    }

    fn query(q: &str, text: &str) -> String {
        let input = serde_json::from_str(text).unwrap();
        serde_json::to_string(&exec(q, input).unwrap()).unwrap()
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
        assert!(exec(".a.x", serde_json::from_str(r#"{"a":1}"#).unwrap()).is_err());
    }

    #[test]
    fn eval_array_op_on_object_errors() {
        assert!(exec("[]", serde_json::from_str(r#"{"a":1}"#).unwrap()).is_err());
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
        assert!(exec("[1:2]", serde_json::from_str(r#"{"a":1}"#).unwrap()).is_err());
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
                exec("/a/", serde_json::from_str(text).unwrap()).is_err(),
                "expected error for {text}"
            );
        }
    }

    #[test]
    fn eval_pick_keeps_listed_properties_in_listed_order() {
        let text = r#"{"a":1,"b":{"n":2},"c":3}"#;
        assert_eq!(query("{c,a}", text), r#"[{"c":3,"a":1}]"#);
        assert_eq!(query("{b}", text), r#"[{"b":{"n":2}}]"#);
        assert_eq!(query("{a,nope}", text), r#"[{"a":1,"nope":null}]"#);
    }

    #[test]
    fn eval_pick_from_each_object() {
        let text = r#"{"x":{"name":"foo","age":33,"h":[1]},"y":{"name":"bar","age":29,"h":[2]}}"#;
        assert_eq!(
            query("*.{name,age}", text),
            r#"[{"name":"foo","age":33},{"name":"bar","age":29}]"#
        );
    }

    #[test]
    fn eval_pick_then_property() {
        let text = r#"{"a":1,"b":2}"#;
        assert_eq!(query("{b,a}.a", text), "[1]");
    }

    #[test]
    fn eval_pick_on_null_gives_nulls() {
        assert_eq!(query(".nope.{a,b}", "{}"), r#"[{"a":null,"b":null}]"#);
    }

    #[test]
    fn execute_pick_on_non_object_errors() {
        for input in [json!([{"a": 1}]), json!(1), json!("s")] {
            assert_eq!(
                exec("{a,b}", input.clone()),
                Err(ExecError::NotAnObject),
                "on {input}"
            );
        }
    }

    #[test]
    fn single_or_array_unwraps_only_a_single_value() {
        assert_eq!(single_or_array(vec![json!([1])]), json!([1]));
        assert_eq!(single_or_array(vec![json!(1), json!(2)]), json!([1, 2]));
        assert_eq!(single_or_array(vec![]), json!([]));
    }

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
    fn eval_keys_in_document_order() {
        assert_eq!(query(".^", r#"{"b":1,"a":2,"c":3}"#), r#"["b","a","c"]"#);
        assert_eq!(query(".^", "{}"), "[]");
    }

    #[test]
    fn eval_keys_of_each_object() {
        let text = r#"{"x":{"a":1,"b":2},"y":{"c":3}}"#;
        assert_eq!(query("*.^", text), r#"["a","b","c"]"#);
    }

    #[test]
    fn eval_steps_after_keys_build_one_object() {
        let text = r#"{"Tim":{"age":53},"Fred":{"age":50,"hobbies":["a","b"]}}"#;
        assert_eq!(query(".^.age", text), r#"[{"Tim":53,"Fred":50}]"#);
        assert_eq!(
            query(".^.{age}", text),
            r#"[{"Tim":{"age":53},"Fred":{"age":50}}]"#
        );
        assert_eq!(
            query(
                ".^.hobbies.[]",
                r#"{"Fred":{"hobbies":["a","b"]},"Ann":{"hobbies":[]}}"#
            ),
            r#"[{"Fred":["a","b"],"Ann":[]}]"#
        );
        assert_eq!(
            query(".^.hobbies.[]", r#"{"Fred":{"hobbies":["a"]}}"#),
            r#"[{"Fred":"a"}]"#
        );
    }

    #[test]
    fn eval_steps_after_keys_build_one_object_per_input() {
        let text = r#"{"x":{"a":{"n":1},"b":{"n":2}},"y":{"c":{"n":3}}}"#;
        assert_eq!(query("*.^.n", text), r#"[{"a":1,"b":2},{"c":3}]"#);
    }

    #[test]
    fn eval_nested_keys() {
        let text = r#"{"x":{"a":1,"b":2},"y":{"c":3},"z":{}}"#;
        assert_eq!(query(".^.^", text), r#"[{"x":["a","b"],"y":"c","z":[]}]"#);
    }

    #[test]
    fn execute_steps_after_keys_propagate_errors() {
        assert_eq!(exec(".^.a", json!({"x": 1})), Err(ExecError::NotAnObject));
        assert_eq!(exec(".^.a", json!([1])), Err(ExecError::NotAnObject));
    }

    #[test]
    fn execute_keys_on_non_object_is_not_an_object() {
        for input in [json!([1]), json!(null), json!(1), json!("s")] {
            assert_eq!(
                exec("^", input.clone()),
                Err(ExecError::NotAnObject),
                "on {input}"
            );
        }
    }

    #[test]
    fn exec_error_display() {
        assert_eq!(ExecError::NotAnObject.to_string(), "not an object");
        assert_eq!(ExecError::NotAnArray.to_string(), "not an array");
    }
}
