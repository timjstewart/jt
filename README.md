# jt

A small command-line tool for pulling values out of JSON, in the spirit of `jq`.

```sh
jt <query> [file]
```

`jt` reads JSON from `file`, or from stdin if no file is given, runs the query, and prints the results as a single pretty-printed JSON array:

```sh
$ jt '.Fred.hobbies.[]' people.json
[
  "bridge",
  "yodelling",
  "chess"
]
```

```sh
jt '.Tim.age' people.json
cat people.json | jt '.Tim.age'
```

Always quote the query. Characters like `*`, `[` and `$` mean something to the shell.

## Building

```sh
cargo build --release
./target/release/jt '.Tim' people.json
```

## Sample data

Every example below runs against this file, `people.json`:

```json
{
  "Tim": { "age": 53 },
  "Fred": {
    "age": 50,
    "hobbies": ["bridge", "yodelling", "chess"]
  },
  "user_1": { "name": "ann" },
  "user_2": { "name": "bob" }
}
```

## Query syntax

To save space, the example outputs below are shown on one line.


A query is a list of steps separated by `.`. The leading `.` is optional, so `.Tim.age` and `Tim.age` mean the same thing. Each step is applied to every value produced by the step before it, so one step can fan out into many results.

| Step | Applies to | Result |
|---|---|---|
| `name` | object | the value of property `name` |
| `*` | object | every property value |
| `^` | object | every property key |
| `/regex/` | object | every property value whose key matches `regex` |
| `[]` | array | every element |
| `[n]` | array | the element at index `n` |
| `[start:stop]` | array | a sub-array, like a Python slice |

An empty query (`.` or `''`) prints the input unchanged, without wrapping it in an array. It works as a JSON pretty-printer: `jt . data.json`.

### Properties: `name`

| Query | Output |
|---|---|
| `.Tim` | `[{"age":53}]` |
| `.Tim.age` | `[53]` |
| `.Fred.hobbies` | `[["bridge","yodelling","chess"]]` |
| `.nobody` | `[null]` |
| `.nobody.age` | `[null]` |

A missing property gives `null`, and looking up a property of `null` also gives `null`, so a missing link part-way along a path doesn't cause an error.

Property names may contain only letters, `_` and `-`. To reach a key with other characters, such as `user_1`, use a regex: `/^user_1$/`.

### All properties: `*`

| Query | Output |
|---|---|
| `*` | `[{"age":53},{"age":50,"hobbies":[...]},{"name":"ann"},{"name":"bob"}]` |
| `*.age` | `[53,50,null,null]` |

Values come back in the order they appear in the document.

### All keys: `^`

| Query | Output |
|---|---|
| `.^` | `["Tim","Fred","user_1","user_2"]` |
| `.Fred.^` | `["age","hobbies"]` |
| `*.^` | `["age","age","hobbies","name","name"]` |

Keys come back as strings, in document order. As with `*`, each key is a separate result, so `*.^` gives the keys of every object in one list, repeats included.

`^` must be the last step. Keys are strings, and no step applies to a string, so a query like `.^.x` is rejected before any input is read.

### Properties matching a regex: `/regex/`

| Query | Output |
|---|---|
| `/^user_/` | `[{"name":"ann"},{"name":"bob"}]` |
| `/^user_/.name` | `["ann","bob"]` |
| `/^(Tim\|Fred)$/.age` | `[53,50]` |

- The regex only has to match **part** of the key. Use `^` and `$` to match the whole key.
- Dots inside the slashes belong to the regex. `/a.b/.c` is the regex `a.b` followed by the property `c`.
- Write a literal `/` as `\/`, e.g. `/a\/b/`.
- The syntax is Rust's [`regex`](https://docs.rs/regex/latest/regex/#syntax) crate syntax.

### Array elements: `[]`

| Query | Output |
|---|---|
| `.Fred.hobbies.[]` | `["bridge","yodelling","chess"]` |

Array steps are written after a `.`, like any other step: `.hobbies.[]`, not `.hobbies[]`.

### Array index: `[n]`

| Query | Output |
|---|---|
| `.Fred.hobbies.[0]` | `["bridge"]` |
| `.Fred.hobbies.[5]` | `[null]` |

Indexes start at 0. An index past the end gives `null`. Negative indexes are not supported, but a slice can do the same job: `[-1:]` is a one-element array holding the last item.

### Array slices: `[start:stop]`

Slices work like Python slices without a step. `start` is included, `stop` is not, and either one can be left out.

| Query | Output |
|---|---|
| `.Fred.hobbies.[1:]` | `[["yodelling","chess"]]` |
| `.Fred.hobbies.[:-1]` | `[["bridge","yodelling"]]` |
| `.Fred.hobbies.[-2:]` | `[["yodelling","chess"]]` |
| `.Fred.hobbies.[1:].[]` | `["yodelling","chess"]` |

- Negative numbers count from the end.
- Bounds past either end are clamped, so `[10:20]` gives `[]` instead of an error.
- A slice returns one array. Follow it with `.[]` to get the elements one by one.

## Errors

`jt` prints errors to stderr as `jt: <message>` and exits with status 1.

| Message | Cause | Example |
|---|---|---|
| `invalid query` | the query doesn't parse | `.Tim..age`, `.Tim.[-1]`, `/(/` |
| `` `^` must be the last step `` | a step after `^` | `.^.x`, `.Fred.^.[0]` |
| `not an object` | a property step on something that isn't an object | `.Tim.age.x`, `.Fred.hobbies.age` |
| `not an array` | an array step on something that isn't an array | `.Tim.[]` |
| file or JSON errors | the file is missing, or the input isn't valid JSON | |

On `null`:
- `*`, `^`, `/regex/` and `[]` are errors.
- `name`, `[n]` and `[start:stop]` return `null`.
