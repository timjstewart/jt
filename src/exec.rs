//! Runs parsed queries against JSON values.

use crate::parser::{Op, PickEntry};
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
///
/// Once a step has fanned out into several results, later steps leave out
/// `null` results, and a property that holds an array adds its elements
/// instead, unless the next step is an array step. So `*.hobbies` gives every
/// hobby, like `*.hobbies[]`, while `*.hobbies[0]` gives each first hobby.
pub fn execute(ops: &[Op], input: Vec<Value>) -> Result<Vec<Value>, ExecError> {
    let mut nodes = input;
    let mut fanned_out = false;
    for (i, op) in ops.iter().enumerate() {
        let mut out = vec![];
        for node in nodes {
            apply(op, node, &mut out)?;
        }
        if fanned_out {
            let flatten =
                matches!(op, Op::Property(_)) && !ops.get(i + 1).is_some_and(Op::is_array_step);
            if flatten {
                out = out.into_iter().flat_map(elements_or_self).collect();
            }
            out.retain(|value| !value.is_null());
        }
        fanned_out |= op.fans_out();
        nodes = out;
    }
    Ok(nodes)
}

/// The elements of an array, or else the value on its own.
fn elements_or_self(value: Value) -> Vec<Value> {
    match value {
        Value::Array(array) => array,
        value => vec![value],
    }
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
        (Op::PropertyPick(entries), Value::Object(obj)) => out.push(pick(entries, obj)?),
        (Op::PropertyValues(pattern), Value::Object(obj)) => out.extend(
            obj.into_iter()
                .filter(|(key, _)| pattern.is_match(key))
                .map(|(_, value)| value),
        ),
        (Op::PropertyKeys(pattern), Value::Object(obj)) => out.extend(
            obj.into_iter()
                .filter(|(key, _)| pattern.is_match(key))
                .map(|(key, _)| Value::String(key)),
        ),
        (Op::PropertyMap(pattern, ops), Value::Object(obj)) => {
            let mapped = obj
                .into_iter()
                .filter(|(key, _)| pattern.is_match(key))
                .map(|(key, value)| Ok((key, collect(ops, execute(ops, vec![value])?))))
                .collect::<Result<_, _>>()?;
            out.push(Value::Object(mapped));
        }
        (Op::Array, Value::Array(array)) => out.extend(array),
        (Op::ArrayIndex(n), Value::Array(array)) => {
            out.push(array.into_iter().nth(*n).unwrap_or_default());
        }
        (Op::ArraySlice { start, stop }, Value::Array(mut array)) => {
            let range = slice_range(array.len(), *start, *stop);
            out.extend(array.drain(range));
        }
        // Looking up a single element of null yields null, as in jq.
        (Op::Property(_) | Op::ArrayIndex(_), Value::Null) => out.push(Value::Null),
        // Every element of null, or a slice of it, is nothing, as for an empty array.
        (Op::Array | Op::ArraySlice { .. }, Value::Null) => {}
        // Every property of null is null, so picking from null gives all nulls, as in jq.
        (Op::PropertyPick(entries), Value::Null) => out.push(pick(entries, Map::new())?),
        (
            Op::Property(_)
            | Op::PropertyPick(_)
            | Op::PropertyKeys(_)
            | Op::PropertyMap(..)
            | Op::PropertyValues(_),
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

/// The results `values` of running `ops` on one value, as a single value: the
/// only result as it is, and otherwise an array of them. A slice in `ops`
/// always gives an array, even of one element.
pub fn collect(ops: &[Op], mut values: Vec<Value>) -> Value {
    let has_slice = ops.iter().any(|op| matches!(op, Op::ArraySlice { .. }));
    if !has_slice && values.len() == 1 {
        values.pop().unwrap_or_default()
    } else {
        Value::Array(values)
    }
}

/// Builds an object holding only the properties of `obj` named in `entries`,
/// in that order, with `null` for any that are missing. Each property's value
/// is the result of running the entry's steps on it.
fn pick(entries: &[PickEntry], mut obj: Map<String, Value>) -> Result<Value, ExecError> {
    entries
        .iter()
        .map(|(name, ops)| {
            let (key, value) = obj
                .remove_entry(name)
                .unwrap_or_else(|| (name.clone(), Value::Null));
            Ok((key, collect(ops, execute(ops, vec![value])?)))
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
        assert_eq!(query("a", r#"{"a":[1,2,3]}"#), "[[1,2,3]]");
    }

    #[test]
    fn eval_nested_property() {
        assert_eq!(query("a.name", r#"{"a":{"name":"foo"}}"#), r#"["foo"]"#);
    }

    #[test]
    fn eval_missing_property_is_null() {
        assert_eq!(query("nope.x", r#"{"a":1}"#), "[null]");
    }

    #[test]
    fn eval_wildcard_then_property() {
        let text = r#"{"b":{"name":"bar"},"a":{"name":"foo"}}"#;
        assert_eq!(query("*.name", text), r#"["bar","foo"]"#);
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
        assert!(exec("a.x", serde_json::from_str(r#"{"a":1}"#).unwrap()).is_err());
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
            execute(&parse("*").unwrap(), vec![input]),
            Ok(vec![Value::from(1), Value::from(2)])
        );
    }

    #[test]
    fn eval_slice_matches_python() {
        // Expected values produced by Python on [0, 1, 2, 3, 4].
        let cases = [
            ("[1:3]", "[1,2]"),
            ("[-2:]", "[3,4]"),
            ("[:-1]", "[0,1,2,3]"),
            ("[10:20]", "[]"),
            ("[-10:2]", "[0,1]"),
            ("[3:1]", "[]"),
            ("[-1:-3]", "[]"),
            ("[:]", "[0,1,2,3,4]"),
            ("[:10]", "[0,1,2,3,4]"),
        ];
        for (q, expected) in cases {
            assert_eq!(query(q, "[0,1,2,3,4]"), expected, "query {q:?}");
        }
    }

    #[test]
    fn eval_slice_gives_each_element() {
        let text = r#"[{"n":"a"},{"n":"b"},{"n":"c"}]"#;
        assert_eq!(query("[1:].n", text), r#"["b","c"]"#);
        assert_eq!(query("[:].n", text), query("[].n", text));
    }

    #[test]
    fn eval_slice_of_nested_arrays_keeps_them_whole() {
        assert_eq!(query("[:1]", "[[1,2],[3]]"), "[[1,2]]");
    }

    #[test]
    fn eval_slice_on_null_is_empty() {
        assert_eq!(query("nope[1:2]", r#"{"a":1}"#), "[]");
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
    fn eval_pick_paths_keep_structure() {
        let text = r#"{"age":50,"hobbies":["bridge","chess"],"last":{"name":"Smith","x":1},"y":2}"#;
        assert_eq!(
            query("{age,hobbies[0],last.name}", text),
            r#"[{"age":50,"hobbies":"bridge","last":{"name":"Smith"}}]"#
        );
        assert_eq!(
            query("{hobbies[1:],last.{x,name}}", text),
            r#"[{"hobbies":["chess"],"last":{"x":1,"name":"Smith"}}]"#
        );
        assert_eq!(
            query("{last.name,age,last.x}", text),
            r#"[{"last":{"name":"Smith","x":1},"age":50}]"#
        );
    }

    #[test]
    fn eval_pick_paths_through_arrays() {
        let text = r#"{"f":[{"n":"a","m":1},{"n":"b"}]}"#;
        assert_eq!(query("{f[].n}", text), r#"[{"f":[{"n":"a"},{"n":"b"}]}]"#);
        assert_eq!(query("{f[0].{n,m}}", text), r#"[{"f":{"n":"a","m":1}}]"#);
    }

    #[test]
    fn eval_pick_paths_on_missing_values() {
        let text = r#"{"age":8}"#;
        assert_eq!(
            query("{hobbies[0],last.name}", text),
            r#"[{"hobbies":null,"last":{"name":null}}]"#
        );
        assert_eq!(
            exec("{age[0]}", json!({"age": 8})),
            Err(ExecError::NotAnArray)
        );
        assert_eq!(
            exec("{age.x}", json!({"age": 8})),
            Err(ExecError::NotAnObject)
        );
    }

    #[test]
    fn eval_pick_paths_after_keys() {
        let text = r#"{"Tim":{"age":53,"hobbies":["chess"]},"Teddy":{"age":8}}"#;
        assert_eq!(
            query("/T.*/^.{age,hobbies[0]}", text),
            r#"[{"Tim":{"age":53,"hobbies":"chess"},"Teddy":{"age":8,"hobbies":null}}]"#
        );
    }

    #[test]
    fn eval_pick_on_null_gives_nulls() {
        assert_eq!(query("nope.{a,b}", "{}"), r#"[{"a":null,"b":null}]"#);
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
    fn collect_unwraps_only_a_single_value_without_a_slice() {
        let ops = |q| parse(q).unwrap();
        assert_eq!(collect(&ops("a"), vec![json!([1])]), json!([1]));
        assert_eq!(collect(&ops("a"), vec![json!(1)]), json!(1));
        assert_eq!(collect(&ops("*"), vec![json!(1)]), json!(1));
        assert_eq!(collect(&ops("[]"), vec![]), json!([]));
        assert_eq!(collect(&ops("*"), vec![json!(1), json!(2)]), json!([1, 2]));
        assert_eq!(collect(&ops("[:]"), vec![json!(1)]), json!([1]));
        assert_eq!(collect(&ops("[0:1].a"), vec![json!(1)]), json!([1]));
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
        assert_eq!(exec("nope[0]", json!({})), Ok(vec![Value::Null]));
    }

    #[test]
    fn execute_slice_of_empty_array() {
        assert_eq!(exec("[1:3]", json!([])), Ok(vec![]));
    }

    #[test]
    fn execute_applies_op_to_every_node() {
        let input = json!([{"n": 1}, {"n": 2}, {"n": 3}]);
        assert_eq!(exec("[].n", input), Ok(vec![json!(1), json!(2), json!(3)]));
    }

    #[test]
    fn eval_property_after_fan_out_drops_nulls() {
        let text = r#"{"a":{"n":1},"b":{},"c":{"n":null},"d":{"n":2}}"#;
        assert_eq!(query("*.n", text), "[1,2]");
        assert_eq!(query("[].n", r#"[{"n":1},{"m":2}]"#), "[1]");
    }

    #[test]
    fn eval_property_after_fan_out_flattens_arrays() {
        let text = r#"{"Tim":{},"Fred":{"h":["a","b"]},"Ann":{"h":["c",null]}}"#;
        assert_eq!(query("*.h", text), r#"["a","b","c"]"#);
        assert_eq!(query("*.h[]", text), query("*.h", text));
        assert_eq!(query("*.h[0]", text), r#"["a","c"]"#);
        assert_eq!(query("*.h[1:]", text), r#"["b"]"#);
        // A value that isn't an array is kept as it is, but `[]` needs an array.
        let text = r#"{"Fred":{"h":["a"]},"Bob":{"h":"d"}}"#;
        assert_eq!(query("*.h", text), r#"["a","d"]"#);
        assert_eq!(
            exec("*.h[]", serde_json::from_str(text).unwrap()),
            Err(ExecError::NotAnArray)
        );
    }

    #[test]
    fn eval_flattens_one_level_then_keeps_going() {
        let text = r#"{"x":{"f":[{"n":1},{"n":2}]},"y":{"f":[[3]]}}"#;
        assert_eq!(query("*.f", text), r#"[{"n":1},{"n":2},[3]]"#);
        assert_eq!(query("/x/.f.n", text), "[1,2]");
    }

    #[test]
    fn eval_property_without_fan_out_is_unchanged() {
        let text = r#"{"h":["a","b"],"n":null}"#;
        assert_eq!(query("h", text), r#"[["a","b"]]"#);
        assert_eq!(query("n", text), "[null]");
        assert_eq!(query("nope", text), "[null]");
    }

    #[test]
    fn eval_steps_after_keys_start_without_fan_out() {
        let text = r#"{"Tim":{},"Fred":{"h":["a","b"]}}"#;
        assert_eq!(query("*^.h", text), r#"[{"Tim":null,"Fred":["a","b"]}]"#);
        assert_eq!(query("*^.h[]", text), r#"[{"Tim":[],"Fred":["a","b"]}]"#);
        assert_eq!(
            query("*^.{h}", text),
            r#"[{"Tim":{"h":null},"Fred":{"h":["a","b"]}}]"#
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
        for q in ["a", "*", "/a/"] {
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
    fn execute_iteration_ops_on_null() {
        assert_eq!(exec("x[]", json!({})), Ok(vec![]));
        assert_eq!(exec("x.*", json!({})), Err(ExecError::NotAnObject));
        assert_eq!(exec("x./a/", json!({})), Err(ExecError::NotAnObject));
    }

    #[test]
    fn execute_object_ops_on_array_are_not_an_object() {
        for q in ["a", "*", "/a/"] {
            assert_eq!(
                exec(q, json!([{"a": 1}])),
                Err(ExecError::NotAnObject),
                "query {q:?}"
            );
        }
    }

    #[test]
    fn eval_wildcard_is_regex_matching_everything() {
        let text = r#"{"b":{"n":1},"a":{"n":2},"":{"n":3}}"#;
        for (wildcard, regex) in [("*", "/.*/"), ("*^", "/.*/^"), ("*^.n", "/.*/^.n")] {
            assert_eq!(
                query(wildcard, text),
                query(regex, text),
                "{wildcard} vs {regex}"
            );
        }
    }

    #[test]
    fn eval_name_keys_match_like_regex_keys() {
        let text = r#"{"Tim":{"age":53},"Timothy":{"age":20},"Fred":{"age":50}}"#;
        assert_eq!(query("Tim^", text), r#"["Tim","Timothy"]"#);
        assert_eq!(query("Tim^.age", text), r#"[{"Tim":53,"Timothy":20}]"#);
        for (name, regex) in [
            ("Tim^", "/Tim/^"),
            ("Tim^.age", "/Tim/^.age"),
            ("zz^", "/zz/^"),
        ] {
            assert_eq!(query(name, text), query(regex, text), "{name} vs {regex}");
        }
    }

    #[test]
    fn eval_keys_in_document_order() {
        assert_eq!(query("*^", r#"{"b":1,"a":2,"c":3}"#), r#"["b","a","c"]"#);
        assert_eq!(query("*^", "{}"), "[]");
    }

    #[test]
    fn eval_keys_of_each_object() {
        let text = r#"{"x":{"a":1,"b":2},"y":{"c":3}}"#;
        assert_eq!(query("*.*^", text), r#"["a","b","c"]"#);
    }

    #[test]
    fn eval_steps_after_keys_build_one_object() {
        let text = r#"{"Tim":{"age":53},"Fred":{"age":50,"hobbies":["a","b"]}}"#;
        assert_eq!(query("*^.age", text), r#"[{"Tim":53,"Fred":50}]"#);
        assert_eq!(
            query("*^.{age}", text),
            r#"[{"Tim":{"age":53},"Fred":{"age":50}}]"#
        );
        assert_eq!(
            query(
                "*^.hobbies[]",
                r#"{"Fred":{"hobbies":["a","b"]},"Ann":{"hobbies":[]}}"#
            ),
            r#"[{"Fred":["a","b"],"Ann":[]}]"#
        );
        assert_eq!(
            query("*^.hobbies[]", r#"{"Fred":{"hobbies":["a"]}}"#),
            r#"[{"Fred":"a"}]"#
        );
    }

    #[test]
    fn eval_steps_after_keys_build_one_object_per_input() {
        let text = r#"{"x":{"a":{"n":1},"b":{"n":2}},"y":{"c":{"n":3}}}"#;
        assert_eq!(query("*.*^.n", text), r#"[{"a":1,"b":2},{"c":3}]"#);
    }

    #[test]
    fn eval_nested_keys() {
        let text = r#"{"x":{"a":1,"b":2},"y":{"c":3},"z":{}}"#;
        assert_eq!(query("*^.*^", text), r#"[{"x":["a","b"],"y":"c","z":[]}]"#);
    }

    #[test]
    fn execute_steps_after_keys_propagate_errors() {
        assert_eq!(exec("*^.a", json!({"x": 1})), Err(ExecError::NotAnObject));
        assert_eq!(exec("*^.a", json!([1])), Err(ExecError::NotAnObject));
    }

    #[test]
    fn eval_regex_keys_lists_matching_keys() {
        let text = r#"{"Tim":{"age":53},"Fred":{"age":50},"Tom":{}}"#;
        assert_eq!(query("/^T/^", text), r#"["Tim","Tom"]"#);
        assert_eq!(query("/zzz/^", text), "[]");
    }

    #[test]
    fn eval_steps_after_regex_keys_build_object_of_matching_keys() {
        let text = r#"{"Tim":{"name":"t","age":53},"Fred":{"name":"f"},"Tom":{"name":"o"}}"#;
        assert_eq!(query("/T.*/^.name", text), r#"[{"Tim":"t","Tom":"o"}]"#);
        assert_eq!(query("/^Tim$/^.{age}", text), r#"[{"Tim":{"age":53}}]"#);
        assert_eq!(query("/zzz/^.name", text), "[{}]");
    }

    #[test]
    fn eval_first_hobby_of_matching_keys() {
        let text = r#"{"Tim":{"hobbies":["chess","golf"]},"Teddy":{"hobbies":[]},"Tom":{},"Fred":{"hobbies":["bridge"]}}"#;
        assert_eq!(
            query("/T.*/^.hobbies[0]", text),
            r#"[{"Tim":"chess","Teddy":null,"Tom":null}]"#
        );
    }

    #[test]
    fn eval_slice_after_keys_is_always_an_array() {
        let text = r#"{"Tim":{"hobbies":["chess","golf"]},"Teddy":{"hobbies":["kites"]},"Tom":{}}"#;
        assert_eq!(
            query("/T.*/^.hobbies[0:1]", text),
            r#"[{"Tim":["chess"],"Teddy":["kites"],"Tom":[]}]"#
        );
        assert_eq!(
            query("/T.*/^.hobbies[:]", text),
            r#"[{"Tim":["chess","golf"],"Teddy":["kites"],"Tom":[]}]"#
        );
    }

    #[test]
    fn eval_nested_regex_keys() {
        let text = r#"{"x1":{"a1":1,"b":2},"y":{"a2":3}}"#;
        assert_eq!(query("/x/^./a/^", text), r#"[{"x1":"a1"}]"#);
    }

    #[test]
    fn execute_regex_keys_on_non_object_is_not_an_object() {
        for q in ["/a/^", "/a/^.b"] {
            assert_eq!(
                exec(q, json!([1])),
                Err(ExecError::NotAnObject),
                "query {q:?}"
            );
        }
    }

    #[test]
    fn execute_keys_on_non_object_is_not_an_object() {
        for input in [json!([1]), json!(null), json!(1), json!("s")] {
            assert_eq!(
                exec("*^", input.clone()),
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
