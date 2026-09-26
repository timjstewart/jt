# jt

A small command-line tool for pulling values out of JSON, in the spirit of `jq`.

```sh
jt [-C|--color] [--keep-nulls] <query> [file]
```

`jt` reads JSON from `file`, or from stdin if no file is given, runs the query, and pretty-prints the result. A query that gives one result prints it on its own. A query that gives several, or none, prints them as a JSON array. A query with `[]` or a slice (`[start:stop]`) asks for array elements, so it always prints an array, even of one element:

```sh
$ jt 'Fred.hobbies[]' people.json
[
  "bridge",
  "yodelling",
  "chess"
]
$ jt 'Fred.age' people.json
50
```

```sh
jt 'Tim.age' people.json
cat people.json | jt 'Tim.age'
```

Output is colored, except when it is piped or redirected. Pass `-C` (or `--color`) to force color anyway, e.g. `jt -C Tim people.json | less -R`.

Keys whose values are `null` are left out of the output, at any depth, so `{"a":1,"b":null}` prints as `{"a":1}`. Pass `--keep-nulls` to keep them. Nulls in arrays, and a result that is itself `null`, are always printed. The examples below show output without `--keep-nulls`.

`jt --help` lists the options.

Always quote the query. Characters like `*`, `[` and `$` mean something to the shell.

## Building

```sh
cargo build --release
./target/release/jt 'Tim' people.json
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


A query is a list of steps separated by `.`, such as `Tim.age`. A query can't start with a `.`, so `.Tim.age` is an invalid query. Each step is applied to every value produced by the step before it, so one step can fan out into many results.

| Step | Applies to | Result |
|---|---|---|
| `name` | object | the value of property `name` |
| `{a,b}` | object | a new object with only properties `a` and `b` |
| `*` | object | every property value |
| `*^` | object | every property key |
| `/regex/` | object | every property value whose key matches `regex` |
| `/regex/^` | object | every property key that matches `regex` |
| `name^` | object | every property key that contains `name` |
| `**.step` | anything | `step` run on every object at any depth |
| `**^.step` | anything | the input pruned down to what `step` finds at any depth |
| `[]` | array | every element |
| `[n]` | array | the element at index `n` |
| `[start:stop]` | array | every element in a range, like a Python slice |

An empty query (`''`) prints the input unchanged. It works as a JSON pretty-printer: `jt '' data.json`.

### Properties: `name`

| Query | Output |
|---|---|
| `Tim` | `{"age":53}` |
| `Tim.age` | `53` |
| `Fred.hobbies` | `["bridge","yodelling","chess"]` |
| `nobody` | `null` |
| `nobody.age` | `null` |

A missing property gives `null`, and looking up a property of `null` also gives `null`, so a missing link part-way along a path doesn't cause an error.

Property names may contain only letters, `_` and `-`. To reach a key with other characters, such as `user_1`, use a regex: `/^user_1$/`.

### Pick properties: `{a,b}`

| Query | Output |
|---|---|
| `Fred.{age,hobbies}` | `{"age":50,"hobbies":["bridge","yodelling","chess"]}` |
| `*.{age}` | `[{"age":53},{"age":50},{},{}]` |
| `{Tim,Fred}.Tim` | `{"age":53}` |
| `Fred.{age,hobbies[0]}` | `{"age":50,"hobbies":"bridge"}` |
| `Fred.{hobbies[1:]}` | `{"hobbies":["yodelling","chess"]}` |
| `{Tim.age,Fred.hobbies}` | `{"age":53,"hobbies":["bridge","yodelling","chess"]}` |
| `{Fred.{age,hobbies[0]}}` | `{"age":50,"hobbies":"bridge"}` |

- Builds a new object holding only the listed properties, in the order listed.
- A missing property gets the value `null`, so it is left out unless `--keep-nulls` is passed. On `null`, every listed property is `null`.
- Names follow the same rules as `name`. Don't put spaces after the commas.

Each entry in the list can be a path instead of a single name. The key is the last name in the path, and the value is what the path gives, so `{name,work.department}` gives `{"name":...,"department":...}`:

- A path starts with a property name. Array steps may follow any name: `hobbies[0]`, `hobbies[]`, `hobbies[1:]`. They don't change the key, so `{hobbies[0]}` gives `{"hobbies":...}`.
- The value is shaped as described at the top: one result as it is, several in an array, and with `[]` or a slice always in an array.
- A path may continue with `.` and another path. It may also end with a `{...}`, whose entries go straight into the result, so `{last.{first,name}}` is the same as `{last.first,last.name}`.
- Two entries can't have the same key: `{age,age}`, `{Tim.age,Fred.age}` and `{hobbies[0],hobbies[1]}` are invalid. Different keys can come from the same property: `{last,last.name}` is fine.
- Only names, array steps and `{...}` can appear in a path, not `*`, `*^` or `/regex/`.

### All properties: `*`

| Query | Output |
|---|---|
| `*` | `[{"age":53},{"age":50,"hobbies":[...]},{"name":"ann"},{"name":"bob"}]` |
| `*.age` | `[53,50]` |
| `*.hobbies` | `["bridge","yodelling","chess"]` |

Values come back in the order they appear in the document.

#### Properties after a fan-out

Once a step has given several results (`*`, `*^`, `/regex/`, `/regex/^`, `name^`, `**`, `[]` or a slice), later property steps work across all of them:

| Query | Output |
|---|---|
| `*.hobbies` | `["bridge","yodelling","chess"]` |
| `*.hobbies[]` | `["bridge","yodelling","chess"]` |
| `*.hobbies[0]` | `"bridge"` |
| `*.hobbies[1:]` | `["yodelling","chess"]` |

- Missing properties, and properties that are `null`, are left out, so `*.age` gives `[53,50]`.
- Steps after `*^`, `name^` or `/regex/^` leave out objects with no matching keys, instead of giving `{}` for them. So `*./^h/^[0]` gives only `{"hobbies":"bridge"}`, from `Fred`.
- A property that holds an array gives its elements, one level deep, so `*.hobbies` is the same as `*.hobbies[]`.
- When an array step follows the property, the array is kept whole for that step, so `*.hobbies[0]` gives the first hobby of each person who has any.
- Before any fan-out, nothing is left out or flattened: `Fred.hobbies` is one array, and `nobody` is `null`. The steps after `*^` also start afresh for each key, so `*^.hobbies` gives `"Tim":null`, which `--keep-nulls` shows.

### All keys: `*^`

| Query | Output |
|---|---|
| `*^` | `["Tim","Fred","user_1","user_2"]` |
| `Fred.*^` | `["age","hobbies"]` |
| `*.*^` | `["age","age","hobbies","name","name"]` |

`*` gives every value and `*^` every key, just as `/regex/` gives the matching values and `/regex/^` the matching keys. A `^` on its own is an invalid query.

Keys come back as strings, in document order. As with `*`, each key is a separate result, so `*.*^` gives the keys of every object in one list, repeats included.

#### Steps after `*^`

When more steps follow `*^`, the result is a new object with the same keys. Each value is the result of running the remaining steps on that key's old value. An object gives one new object, however many keys it has.

| Query | Output |
|---|---|
| `*^.age` | `{"Tim":53,"Fred":50}`, or with `--keep-nulls`: `{"Tim":53,"Fred":50,"user_1":null,"user_2":null}` |
| `*^.*` | `{"Tim":53,"Fred":[50,["bridge","yodelling","chess"]],"user_1":"ann","user_2":"bob"}` |
| `*^.*^` | `{"Tim":"age","Fred":["age","hobbies"],"user_1":"name","user_2":"name"}` |

- A single result is stored as it is. When the steps give several results, they are collected into an array, as `Fred` shows in `*^.*`. When they give none, the value is `null`, the same as a missing property, so `*^.hobbies[]` gives only `{"Fred":["bridge","yodelling","chess"]}`. When the steps include `[]` or a slice, the value is always an array, even of one element. With a slice it is an array even when empty; with `[]`, no results still gives `null`.
- The remaining steps run on every value, so they must suit all of them. `*^.age[]` fails with `not an array`, because every age is a number.
- A second `*^` in the remaining steps works the same way, one level down.

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

### Keys matching a regex: `/regex/^`

Works like `*^`, but only for keys that match the regex. The regex follows the same rules as in `/regex/`, and the `^` goes straight after the closing `/`.

| Query | Output |
|---|---|
| `/^user_/^` | `["user_1","user_2"]` |
| `/^user_/^.name` | `{"user_1":"ann","user_2":"bob"}` |
| `/^T/^.age` | `{"Tim":53}` |
| `/^(Tim\|Fred)$/^.{age}` | `{"Tim":{"age":53},"Fred":{"age":50}}` |

With no steps after it, you get the matching keys as strings. With more steps after it, you get a new object that keeps only the matching keys, as described in [Steps after `*^`](#steps-after-). Keys that don't match are left out, so the remaining steps only need to suit the values you kept. `/^user_/^.age[]` works where `*^.age[]` fails.

### Keys containing a name: `name^`

A shorter way to write `/name/^` when the pattern is a plain name. It matches the same keys, but without a regex: it checks whether each key contains `name`.

| Query | Output |
|---|---|
| `user^` | `["user_1","user_2"]` |
| `user^.name` | `{"user_1":"ann","user_2":"bob"}` |
| `red^.age` | `{"Fred":50}` |

- Like `/name/`, the name only has to match **part** of the key, so `Tim^` also matches `Timothy`. Use `/^Tim$/^` to match the whole key.
- The name follows the same rules as `name`: letters, `_` and `-`.
- Without the `^`, `name` is a single property, not a pattern.

### Any depth: `**`

| Query | Output |
|---|---|
| `**.age` | `[53,50]` |
| `**.name` | `["ann","bob"]` |
| `**.hobbies[0]` | `"bridge"` |
| `**.nam^` | `["name","name"]` |
| `Fred.**.age` | `50` |

- `**` runs the step after it on every object inside the value, at any depth: the value itself if it's an object, the objects in its properties, the objects in arrays, and so on down.
- The step after `**` must work on objects: `name`, `{a,b}`, `*`, `*^`, `/regex/`, `/regex/^` or `name^`. `**` on its own, `**[0]` and `**.**` are invalid.
- `**` is a fan-out, so the step after it follows the rules in [Properties after a fan-out](#properties-after-a-fan-out): `**.name` leaves out the objects that have no `name`, and `**.hobbies` gives each hobby.
- Results come in document order, with each object's results before those of the objects inside it. If a match holds another match, both are returned, the outer one first: on `{"a":{"a":1}}`, `**.a` gives `[{"a":1},1]`.
- A pick runs on every object, so `**.{name}` gives an object for each one, and those without `name` print as `{}`.
- `**` can't appear inside `{...}`.

#### Where things are: `**^`

`**^` shows where `**` finds things. When steps follow it, the result is a copy of the input pruned down to what the steps find, reached by the same keys as in the input. Each object where they find something holds it, and everything else is left out.

| Query | Output |
|---|---|
| `**^.age` | `{"Tim":{"age":53},"Fred":{"age":50}}` |
| `**^.hobbies[0]` | `{"Fred":{"hobbies":"bridge"}}` |
| `**^./^n/` | `{"user_1":{"name":"ann"},"user_2":{"name":"bob"}}` |
| `**^.{name,age}` | `{"Tim":{"age":53},"Fred":{"age":50},"user_1":{"name":"ann"},"user_2":{"name":"bob"}}` |
| `**^` | `{"Tim":{},"Fred":{},"user_1":{},"user_2":{}}` |

- What the steps find is kept under a key in the object where they found it:
  - After `*`, `/regex/`, `*^`, `name^` or `/regex/^`, under each matching key. Any steps after those run on its value, so on `{"work":{"department":"sales"}}`, `**^.w^.department` gives `{"work":"sales"}`.
  - For a pick, under the pick's own keys. A pick must be the last step.
  - Otherwise under the last name, as in a pick: `**^.work.department` keeps `{"department":...}`.
- An object where the steps find nothing is left out: no results, `null`, or an empty object from `*^`, `name^` or `/regex/^`. A pick's properties that are missing are left out too.
- Matches inside matches are kept, since the outer one holds the inner one: on `{"age":40,"kid":{"age":5}}`, `**^.age` gives the input unchanged.
- An array element in the path is kept under a key like `"[1]"`, so the result is all objects: on `{"L":[{},{"a":1}]}`, `**^.a` gives `{"L":{"[1]":{"a":1}}}`.
- With no steps, `**^` gives every object, emptied: the shape of the input.
- As after `*^`, the steps after `**^` start afresh on each object, so they don't leave out nulls or flatten arrays. The first of them must work on objects.
- All the steps after `**^` belong to it, including any later `**` or `**^`: `**^.work.**.department` keeps the departments anywhere inside each `work`, under `work`.

### Array elements: `[]`

| Query | Output |
|---|---|
| `Fred.hobbies[]` | `["bridge","yodelling","chess"]` |

`[]` always gives an array, even when there is only one element: on `{"h":["golf"]}`, `h[]` gives `["golf"]`, while `h[0]` gives `"golf"`.

Array steps are written straight after the step before them, with no `.`: `hobbies[]`, not `hobbies.[]`. They chain the same way: `a[0][1]`. A query can also start with an array step: `[0]`.

### Array index: `[n]`

| Query | Output |
|---|---|
| `Fred.hobbies[0]` | `"bridge"` |
| `Fred.hobbies[5]` | `null` |

Indexes start at 0. An index past the end gives `null`. Negative indexes are not supported, but a slice can do the same job: `[-1:]` gives the last element, or nothing if the array is empty.

### Array slices: `[start:stop]`

Slices work like Python slices without a step. `start` is included, `stop` is not, and either one can be left out.

| Query | Output |
|---|---|
| `Fred.hobbies[:]` | `["bridge","yodelling","chess"]` |
| `Fred.hobbies[1:]` | `["yodelling","chess"]` |
| `Fred.hobbies[:-1]` | `["bridge","yodelling"]` |
| `Fred.hobbies[-1:]` | `["chess"]` |
| `*.hobbies[:1]` | `["bridge"]` |
| `/^T/^.hobbies[0:1]` | `{"Tim":[]}` |

- Like `[]`, a slice gives each element in the range as a separate result, so later steps apply to each element. `[:]` is the same as `[]`.
- Negative numbers count from the end.
- Bounds past either end are clamped, so `[10:20]` gives no results instead of an error.
- A slice of `null` gives no results, so `*.hobbies[:1]` skips the people without hobbies.
- A query with a slice always gives an array, even of one element or none, including after `*^`: `*^.hobbies[:1]` gives `["bridge"]` for `Fred` and `[]` for everyone else. Use `[n]` for a single element.

## Errors

`jt` prints errors to stderr as `jt: <message>` and exits with status 1. A bad command line, such as a missing query or an unknown option, prints a usage message and exits with status 2.

| Message | Cause | Example |
|---|---|---|
| `invalid query` | the query doesn't parse | `.Tim`, `Tim..age`, `Fred.hobbies.[0]`, `Tim[-1]`, `/(/`, `{Tim,Tim}` |
| `not an object` | a property step on something that isn't an object | `Tim.age.x`, `Fred.hobbies.age` |
| `not an array` | an array step on something that isn't an array | `Tim[]` |
| file or JSON errors | the file is missing, or the input isn't valid JSON | |

On `null`:
- `*`, `*^`, `/regex/`, `/regex/^` and `name^` are errors.
- `**` gives no results, as it does on any value with no objects in it.
- `name` and `[n]` return `null`, which is left out after a fan-out.
- `[]` and `[start:stop]` give no results.
- `{a,b}` returns an object whose listed properties are all `null`. Each path in it runs on `null`, so `{last.name}` gives `{"name":null}`, printed as `{}` without `--keep-nulls`.
