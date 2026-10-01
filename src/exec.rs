//! Runs parsed queries against JSON values, keeping the structure around what
//! they find.
//!
//! A query first works out a [`Mask`] of what to keep, then cuts the input
//! down to it. Working on masks lets several paths through the same value,
//! from `{a,b}` or `**`, be combined before the output is built.

use crate::parser::{Op, Pattern};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ops::Range;

/// What a query keeps of a value.
#[derive(Debug, Clone, PartialEq)]
enum Mask {
    /// The whole value.
    All,
    /// A property the query names that the object doesn't have, which is
    /// shown as `null`.
    Missing,
    /// Some properties of an object, and what is kept of each.
    Object(BTreeMap<String, Mask>, Unwrap),
    /// Some elements of an array, by index, and what is kept of each.
    Array(BTreeMap<usize, Mask>, Unwrap),
}

/// Whether a `!` step replaces an object or array with the values kept in
/// it: the one value on its own, or several in an array.
type Unwrap = bool;

impl Mask {
    /// Keeps everything either mask keeps.
    fn union(self, other: Mask) -> Mask {
        match (self, other) {
            (Mask::All, _) | (_, Mask::All) => Mask::All,
            (Mask::Missing, other) | (other, Mask::Missing) => other,
            (Mask::Object(mut a, ua), Mask::Object(b, ub)) => {
                merge(&mut a, b);
                Mask::Object(a, ua || ub)
            }
            (Mask::Array(mut a, ua), Mask::Array(b, ub)) => {
                merge(&mut a, b);
                Mask::Array(a, ua || ub)
            }
            // The masks of one value are of one kind, since they follow its type.
            (a, _) => a,
        }
    }
}

fn merge<K: Ord>(into: &mut BTreeMap<K, Mask>, from: BTreeMap<K, Mask>) {
    for (key, mask) in from {
        let mask = match into.remove(&key) {
            Some(old) => old.union(mask),
            None => mask,
        };
        into.insert(key, mask);
    }
}

/// Runs the query `ops` on `input`, giving `input` cut down to what the query
/// finds, with the same structure: every object and array on the way to a
/// match is kept, holding only what leads to a match. When nothing is found,
/// gives an empty object or array, or `null` for any other input.
pub fn run(ops: &[Op], input: Value) -> Value {
    match find(ops, &input, true) {
        Some(mask) => keep(input, &mask),
        None => match input {
            Value::Object(_) => Value::Object(Default::default()),
            Value::Array(_) => Value::Array(vec![]),
            _ => Value::Null,
        },
    }
}

/// What `ops` keep of `value`, or `None` if they find nothing in it. A step
/// that doesn't suit the value, such as a name on an array, finds nothing.
///
/// With `fill_missing`, a name that is the last step and matches no key of an
/// object is kept as a [`Mask::Missing`] property, so you can see where it was
/// looked for. `**` doesn't do this, or it would add the name to every object.
fn find(ops: &[Op], value: &Value, fill_missing: bool) -> Option<Mask> {
    let Some((op, rest)) = ops.split_first() else {
        return Some(Mask::All);
    };
    let (op, unwrap) = match op {
        Op::Unwrap(op) => (&**op, true),
        op => (op, false),
    };
    match (op, value) {
        (Op::Keys(pattern), Value::Object(obj)) => {
            let mut found: BTreeMap<_, _> = obj
                .iter()
                .filter(|(key, _)| pattern.is_match(key))
                .filter_map(|(key, value)| Some((key.clone(), find(rest, value, fill_missing)?)))
                .collect();
            if found.is_empty()
                && fill_missing
                && rest.is_empty()
                && let Pattern::Prefix(name) | Pattern::Exact(name) = pattern
                && !obj.contains_key(name)
            {
                found.insert(name.clone(), Mask::Missing);
            }
            (!found.is_empty()).then_some(Mask::Object(found, unwrap))
        }
        (Op::Array, Value::Array(array)) => {
            find_elements(rest, array, 0..array.len(), unwrap, fill_missing)
        }
        (Op::ArrayIndex(n), Value::Array(array)) => {
            let range = *n..(*n + 1).min(array.len());
            find_elements(rest, array, range, unwrap, fill_missing)
        }
        (Op::ArraySlice { start, stop }, Value::Array(array)) => {
            let range = slice_range(array.len(), *start, *stop);
            find_elements(rest, array, range, unwrap, fill_missing)
        }
        (Op::Branches(branches), value) => branches
            .iter()
            .filter_map(|branch| find(branch, value, fill_missing))
            .reduce(Mask::union),
        (Op::Descend, value) => descend(rest, value),
        _ => None,
    }
}

/// What `ops` keep of the elements of `array` in `range`.
fn find_elements(
    ops: &[Op],
    array: &[Value],
    range: Range<usize>,
    unwrap: bool,
    fill_missing: bool,
) -> Option<Mask> {
    let found: BTreeMap<_, _> = range
        .filter_map(|i| Some((i, find(ops, &array[i], fill_missing)?)))
        .collect();
    (!found.is_empty()).then_some(Mask::Array(found, unwrap))
}

/// What `ops` keep of `value` and of every value inside it, at any depth.
fn descend(ops: &[Op], value: &Value) -> Option<Mask> {
    let below = match value {
        Value::Object(obj) => {
            let found: BTreeMap<_, _> = obj
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), descend(ops, value)?)))
                .collect();
            (!found.is_empty()).then_some(Mask::Object(found, false))
        }
        Value::Array(array) => {
            let found: BTreeMap<_, _> = array
                .iter()
                .enumerate()
                .filter_map(|(i, value)| Some((i, descend(ops, value)?)))
                .collect();
            (!found.is_empty()).then_some(Mask::Array(found, false))
        }
        _ => None,
    };
    match (find(ops, value, false), below) {
        (Some(here), Some(below)) => Some(here.union(below)),
        (here, below) => here.or(below),
    }
}

/// `value` cut down to `mask`, in document order.
fn keep(value: Value, mask: &Mask) -> Value {
    match (value, mask) {
        (Value::Object(obj), Mask::Object(keys, unwrap)) => {
            let missing = keys
                .iter()
                .filter(|(_, mask)| **mask == Mask::Missing)
                .map(|(key, _)| (key.clone(), Value::Null));
            let kept = obj
                .into_iter()
                .filter_map(|(key, value)| {
                    let value = keep(value, keys.get(&key)?);
                    Some((key, value))
                })
                .chain(missing);
            if *unwrap {
                unwrapped(kept.map(|(_, value)| value).collect())
            } else {
                Value::Object(kept.collect())
            }
        }
        (Value::Array(array), Mask::Array(items, unwrap)) => {
            let kept: Vec<_> = array
                .into_iter()
                .enumerate()
                .filter_map(|(i, value)| Some(keep(value, items.get(&i)?)))
                .collect();
            if *unwrap {
                unwrapped(kept)
            } else {
                Value::Array(kept)
            }
        }
        // `Mask::All`, the only mask that can apply to any value. A missing
        // property has no value in the input, and is added by its object.
        (value, _) => value,
    }
}

/// One value on its own, or several in an array.
fn unwrapped(mut values: Vec<Value>) -> Value {
    if values.len() == 1 {
        values.pop().unwrap_or_default()
    } else {
        Value::Array(values)
    }
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

    /// The result of `q` on the JSON `text`, as compact JSON.
    fn query(q: &str, text: &str) -> String {
        run(&parse(q).unwrap(), serde_json::from_str(text).unwrap()).to_string()
    }

    const PEOPLE: &str = r#"{"Tim":{"age":53},"Fred":{"age":50,"hobbies":["bridge","yodelling","chess"]},"user_1":{"name":"ann"},"user_2":{"name":"bob"}}"#;

    #[test]
    fn empty_query_gives_the_input() {
        assert_eq!(query("", PEOPLE), PEOPLE);
        assert_eq!(query("", "7"), "7");
    }

    #[test]
    fn names_keep_the_path_to_what_they_find() {
        assert_eq!(query("Tim", PEOPLE), r#"{"Tim":{"age":53}}"#);
        assert_eq!(query("Tim.age", PEOPLE), r#"{"Tim":{"age":53}}"#);
        assert_eq!(
            query("F.h", PEOPLE),
            r#"{"Fred":{"hobbies":["bridge","yodelling","chess"]}}"#
        );
        assert_eq!(
            query("user.n", PEOPLE),
            r#"{"user_1":{"name":"ann"},"user_2":{"name":"bob"}}"#
        );
        assert_eq!(query("user_1", PEOPLE), r#"{"user_1":{"name":"ann"}}"#);
    }

    #[test]
    fn names_match_prefixes_and_dollar_matches_exactly() {
        let text = r#"{"foo":{"bar":1,"baz":2},"food":{"bart":3},"xfoo":{"bar":4}}"#;
        assert_eq!(
            query("foo.bar", text),
            r#"{"foo":{"bar":1},"food":{"bart":3}}"#
        );
        assert_eq!(query("foo$.bar", text), r#"{"foo":{"bar":1}}"#);
        assert_eq!(
            query("fo.bar$", text),
            r#"{"foo":{"bar":1},"food":{"bar":null}}"#
        );
        assert_eq!(query("foo$.ba", text), r#"{"foo":{"bar":1,"baz":2}}"#);
    }

    #[test]
    fn paths_that_find_nothing_are_left_out() {
        assert_eq!(
            query("*./age/", PEOPLE),
            r#"{"Tim":{"age":53},"Fred":{"age":50}}"#
        );
        // Steps that don't suit a value find nothing in it, rather than failing.
        assert_eq!(query("*.age.x", PEOPLE), "{}");
        assert_eq!(query("a.b", r#"{"a":null,"c":{"b":1}}"#), "{}");
        assert_eq!(
            query("*.b", r#"{"a":[{"b":1}],"c":{"b":2}}"#),
            r#"{"c":{"b":2}}"#
        );
        assert_eq!(query("[0]", r#"{"a":1}"#), "{}");
    }

    #[test]
    fn a_last_name_that_matches_nothing_is_shown_as_null() {
        let text = r#"{"Tim":{"name":"a"},"Ted":{"name":"b","nap":1},"x":2}"#;
        assert_eq!(
            query("*.age", PEOPLE),
            r#"{"Tim":{"age":53},"Fred":{"age":50},"user_1":{"age":null},"user_2":{"age":null}}"#
        );
        assert_eq!(
            query("T.nar", text),
            r#"{"Tim":{"nar":null},"Ted":{"nar":null}}"#
        );
        assert_eq!(
            query("T.na", text),
            r#"{"Tim":{"name":"a"},"Ted":{"name":"b","nap":1}}"#
        );
        assert_eq!(query("Ted.nar$", text), r#"{"Ted":{"nar":null}}"#);
        assert_eq!(query("nar", text), r#"{"nar":null}"#);
        assert_eq!(query("T.nar!", text), r#"{"Tim":null,"Ted":null}"#);
        assert_eq!(
            query("Ted.{nap,zz}", text),
            r#"{"Ted":{"nap":1,"zz":null}}"#
        );
        // Only the last step, only names, and not under `**`.
        assert_eq!(query("T.nar.x", text), "{}");
        assert_eq!(query("T./zz/", text), "{}");
        assert_eq!(query("T.name.zz", text), "{}");
        assert_eq!(query("**.zz", text), "{}");
        assert_eq!(query("[].zz", r#"[{"a":1},2]"#), r#"[{"zz":null}]"#);
    }

    #[test]
    fn nothing_found_gives_an_empty_value() {
        assert_eq!(query("nobody.x", PEOPLE), "{}");
        assert_eq!(query("[].x", "[1,2]"), "[]");
        assert_eq!(query("x", "1"), "null");
        assert_eq!(query("*.x", "{}"), "{}");
    }

    #[test]
    fn values_that_are_found_are_kept_even_when_null_or_empty() {
        assert_eq!(query("a", r#"{"a":null,"b":1}"#), r#"{"a":null}"#);
        assert_eq!(
            query("**.e", r#"{"e":{},"a":{"e":[]}}"#),
            r#"{"e":{},"a":{"e":[]}}"#
        );
    }

    #[test]
    fn wildcard_and_regex_select_keys() {
        assert_eq!(query("*", PEOPLE), PEOPLE);
        assert_eq!(query("/^user_/.name", PEOPLE), query("user.name", PEOPLE));
        assert_eq!(
            query("/^(Tim|Fred)$/.age", PEOPLE),
            r#"{"Tim":{"age":53},"Fred":{"age":50}}"#
        );
        assert_eq!(query("/zzz/", PEOPLE), "{}");
    }

    #[test]
    fn keeps_document_order() {
        assert_eq!(query("{b,a}", r#"{"a":1,"b":2,"c":3}"#), r#"{"a":1,"b":2}"#);
    }

    #[test]
    fn array_steps_keep_the_elements_they_find_in_an_array() {
        assert_eq!(
            query("Fred.h[0]", PEOPLE),
            r#"{"Fred":{"hobbies":["bridge"]}}"#
        );
        assert_eq!(
            query("Fred.h[1:]", PEOPLE),
            r#"{"Fred":{"hobbies":["yodelling","chess"]}}"#
        );
        assert_eq!(query("Fred.h[5]", PEOPLE), "{}");
        assert_eq!(query("Fred.h[]", PEOPLE), query("Fred.h", PEOPLE));
        let text = r#"[{"name":1},{"x":2},{"name":3,"y":4}]"#;
        assert_eq!(query("[]./name/", text), r#"[{"name":1},{"name":3}]"#);
        assert_eq!(
            query("[].name", text),
            r#"[{"name":1},{"name":null},{"name":3}]"#
        );
        assert_eq!(query("[1:]./name/", text), r#"[{"name":3}]"#);
        assert_eq!(query("[]", "[]"), "[]");
    }

    #[test]
    fn slices_match_python() {
        // Expected values produced by Python on [0, 1, 2, 3, 4].
        let cases = [
            ("[1:3]", "[1,2]"),
            ("[-2:]", "[3,4]"),
            ("[:-1]", "[0,1,2,3]"),
            ("[10:20]", "[]"),
            ("[-10:2]", "[0,1]"),
            ("[3:1]", "[]"),
            ("[:]", "[0,1,2,3,4]"),
        ];
        for (q, expected) in cases {
            assert_eq!(query(q, "[0,1,2,3,4]"), expected, "query {q:?}");
        }
    }

    #[test]
    fn branches_keep_every_path() {
        assert_eq!(
            query("Fred.{age,h[0]}", PEOPLE),
            r#"{"Fred":{"age":50,"hobbies":["bridge"]}}"#
        );
        assert_eq!(
            query("{Tim,user_2}.{a,n}", PEOPLE),
            r#"{"Tim":{"age":53,"n":null},"user_2":{"name":"bob","a":null}}"#
        );
        // Paths into the same array are combined.
        assert_eq!(
            query("Fred.h.{[2],[0]}", PEOPLE),
            r#"{"Fred":{"hobbies":["bridge","chess"]}}"#
        );
        assert_eq!(query("{Tim,Tim.age}", PEOPLE), query("Tim", PEOPLE));
    }

    #[test]
    fn descend_keeps_every_match_at_any_depth() {
        let text =
            r#"{"d":0,"T":{"w":{"d":"acc"}},"L":[{"d":null},{"x":1},{"w":{"d":"fin"}}],"x":{}}"#;
        assert_eq!(
            query("**.d", text),
            r#"{"d":0,"T":{"w":{"d":"acc"}},"L":[{"d":null},{"w":{"d":"fin"}}]}"#
        );
        assert_eq!(
            query("**.w.d", text),
            r#"{"T":{"w":{"d":"acc"}},"L":[{"w":{"d":"fin"}}]}"#
        );
        assert_eq!(
            query("L.**.d", text),
            r#"{"L":[{"d":null},{"w":{"d":"fin"}}]}"#
        );
        assert_eq!(query("**.nope", text), "{}");
        // A match inside a match is kept whole by the outer one.
        assert_eq!(query("**.a", r#"{"a":{"a":1},"b":2}"#), r#"{"a":{"a":1}}"#);
        assert_eq!(query("**[1]", r#"{"a":[1,[2,3]]}"#), r#"{"a":[[2,3]]}"#);
    }

    #[test]
    fn unwrap_leaves_a_step_out_of_the_path() {
        let text =
            r#"{"L":{"coll":{"stamp":{"count":1},"cards":{"count":2}}},"T":{"cars":[{"n":1}]}}"#;
        assert_eq!(query("**.car!.cou", text), r#"{"L":{"coll":{"count":2}}}"#);
        assert_eq!(
            query("L.c!.*!.count", text),
            r#"{"L":[{"count":1},{"count":2}]}"#
        );
        assert_eq!(query("T.cars![0]!.n", text), r#"{"T":{"n":1}}"#);
        assert_eq!(
            query("{L.c.s!,T}", text),
            r#"{"L":{"coll":{"count":1}},"T":{"cars":[{"n":1}]}}"#
        );
    }

    #[test]
    fn unwrap_at_the_end_replaces_the_last_level_with_its_values() {
        assert_eq!(query("Fred.age!", PEOPLE), r#"{"Fred":50}"#);
        assert_eq!(
            query("*.age!", PEOPLE),
            r#"{"Tim":53,"Fred":50,"user_1":null,"user_2":null}"#
        );
        assert_eq!(
            query("user.name!", PEOPLE),
            r#"{"user_1":"ann","user_2":"bob"}"#
        );
        assert_eq!(query("Tim!", PEOPLE), r#"{"age":53}"#);
        assert_eq!(
            query("Fred.h[0]!", PEOPLE),
            r#"{"Fred":{"hobbies":"bridge"}}"#
        );
        assert_eq!(
            query("Fred.h[1:]!", PEOPLE),
            r#"{"Fred":{"hobbies":["yodelling","chess"]}}"#
        );
        assert_eq!(
            query("**.name!", PEOPLE),
            r#"{"user_1":"ann","user_2":"bob"}"#
        );
        // Several values in one object are collected into an array.
        assert_eq!(
            query("Fred.*!", PEOPLE),
            r#"{"Fred":[50,["bridge","yodelling","chess"]]}"#
        );
        assert_eq!(query("Fred.{age!,h!}", PEOPLE), query("Fred.*!", PEOPLE));
        assert_eq!(query("nobody!", PEOPLE), "null");
    }
}
