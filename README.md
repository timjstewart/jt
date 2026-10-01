# jt

A small command-line tool for filtering JSON down to the parts you want, while keeping its structure.

```sh
jt [-C|--color] [--drop-nulls] <query> [file]
```

`jt` reads JSON from `file`, or from stdin if no file is given, runs the query, and pretty-prints the result. The result is the input cut down to what the query finds. Every object and array on the way to a match is kept, holding only what leads to a match:

```sh
$ jt 'Fred.age' people.json
{
  "Fred": {
    "age": 50
  }
}
$ jt 'Fred.age!' people.json
{
  "Fred": 50
}
```

```sh
jt 'Tim.age' people.json
cat people.json | jt 'Tim.age'
```

Output is colored, except when it is piped or redirected. Pass `-C` (or `--color`) to force color anyway, e.g. `jt -C Tim people.json | less -R`.

Pass `--drop-nulls` to leave out keys whose values are `null`, at any depth, so `{"a":1,"b":null}` prints as `{"a":1}`. Nulls in arrays, and a result that is itself `null`, are always printed.

`jt --help` lists the options. To build a query interactively, seeing its result as you type, run `jti people.json` (it needs [`fzf`](https://github.com/junegunn/fzf)).

Always quote the query. Characters like `*`, `[`, `$` and `!` mean something to the shell.

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

A query is a list of steps separated by `.`, such as `Tim.age`. Each step selects parts of what the step before it selected: properties of an object, or elements of an array. The result keeps everything on the path to each part the last step selects.

| Step | Applies to | Selects |
|---|---|---|
| `name` | object | every property whose key starts with `name` |
| `name$` | object | the property whose key is exactly `name` |
| `*` | object | every property |
| `/regex/` | object | every property whose key matches `regex` |
| `[]` | array | every element |
| `[n]` | array | the element at index `n` |
| `[start:stop]` | array | the elements in a range, like a Python slice |
| `{a,b.c}` | anything | everything each path in the braces selects |
| `**.steps` | anything | everything `steps` select, at any depth |

A `!` after a step leaves that step's keys out of the result: see [Leaving keys out: `!`](#leaving-keys-out-).

- An empty query (`''`) prints the input unchanged. It works as a JSON pretty-printer: `jt '' data.json`.
- A query can't start with a `.`, so `.Tim.age` is an invalid query. It may end with one, which is ignored, so a query stays valid while you type it: `Tim.` is the same as `Tim`. For the same reason, an array step left open at the very end is closed for you: `Fred.h[` is the same as `Fred.h[]`, `Fred.h[1` as `Fred.h[1]`, and `Fred.h[1:` as `Fred.h[1:]`.
- A step that doesn't suit a value, such as a name on an array or on a number, selects nothing from it. That path is left out, rather than causing an error.
- When a query selects nothing, the result is `{}` for an object, `[]` for an array, and `null` for anything else.

### Properties: `name` and `name$`

A name selects every key that **starts with** it, so you only need to type enough of each key to pick it out. Add a `$` to select only the key that is exactly that name.

| Query | Output |
|---|---|
| `Tim` | `{"Tim":{"age":53}}` |
| `Tim.age` | `{"Tim":{"age":53}}` |
| `F.h` | `{"Fred":{"hobbies":["bridge","yodelling","chess"]}}` |
| `user.n` | `{"user_1":{"name":"ann"},"user_2":{"name":"bob"}}` |
| `user_1` | `{"user_1":{"name":"ann"}}` |
| `nobody` | `{"nobody":null}` |
| `nobody.age` | `{}` |

- `name` is the same as `/^name/`: `Tim` also selects `Timothy`. Matching is case-sensitive.
- Use `name$` when one key is the start of another: on `{"foo":{"bar":1},"food":{"bart":2}}`, `foo.bar` gives both, and `foo$.bar` only `{"foo":{"bar":1}}`.
- Names may contain only letters, digits, `_` and `-`. To reach a key with other characters, use a regex: `/^first name$/`.
- A key that is selected is kept even when its value is `null`.
- When the last step is a name that matches no key in an object, it is shown there with the value `null`, so you can see where it was looked for: `T.nar` gives `{"Tim":{"nar":null}}`. This only happens for the last step, and not under `**`, where it would add the name to every object.

### All properties: `*`

| Query | Output |
|---|---|
| `*.age` | `{"Tim":{"age":53},"Fred":{"age":50},"user_1":{"age":null},"user_2":{"age":null}}` |
| `*.age.x` | `{}` |
| `*./^age$/` | `{"Tim":{"age":53},"Fred":{"age":50}}` |
| `Fred.*` | `{"Fred":{"age":50,"hobbies":["bridge","yodelling","chess"]}}` |

Properties where the rest of the query finds nothing are left out. The exception is a name as the last step, which is shown as `null` where it is missing, so `*.age` gives the users `"age":null`. To leave them out, use a regex as the last step: `*./^age$/`.

### Properties matching a regex: `/regex/`

| Query | Output |
|---|---|
| `/^user_/` | `{"user_1":{"name":"ann"},"user_2":{"name":"bob"}}` |
| `/^(Tim\|Fred)$/.age` | `{"Tim":{"age":53},"Fred":{"age":50}}` |
| `/e/` | `{"Fred":{"age":50,"hobbies":["bridge","yodelling","chess"]},"user_1":{"name":"ann"},"user_2":{"name":"bob"}}` |

- The regex only has to match **part** of the key. Use `^` and `$` to match the whole key.
- Dots inside the slashes belong to the regex. `/a.b/.c` is the regex `a.b` followed by the name `c`.
- Write a literal `/` as `\/`, e.g. `/a\/b/`.
- The syntax is Rust's [`regex`](https://docs.rs/regex/latest/regex/#syntax) crate syntax.

### Array elements: `[]`, `[n]` and `[start:stop]`

Array steps keep the elements they select in an array, in their original order, leaving the others out.

| Query | Output |
|---|---|
| `Fred.h[]` | `{"Fred":{"hobbies":["bridge","yodelling","chess"]}}` |
| `Fred.h[0]` | `{"Fred":{"hobbies":["bridge"]}}` |
| `Fred.h[1:]` | `{"Fred":{"hobbies":["yodelling","chess"]}}` |
| `Fred.h[-1:]` | `{"Fred":{"hobbies":["chess"]}}` |
| `Fred.h[5]` | `{}` |

- Array steps are written straight after the step before them, with no `.`: `hobbies[0]`, not `hobbies.[0]`. They chain the same way: `a[0][1]`. A query can also start with an array step: `[0]`.
- `[]` selects every element, so it only matters when more steps follow: on `[{"name":1},{"x":2}]`, `[].name` gives `[{"name":1}]`.
- Indexes start at 0. Negative indexes are not supported, but a slice can do the same job: `[-1:]` selects the last element.
- Slices work like Python slices without a step. `start` is included, `stop` is not, either one can be left out, and negative numbers count from the end. Bounds past either end are clamped.

### Several paths: `{a,b.c}`

The braces hold paths separated by commas. The result keeps everything any of them selects. Steps after the braces continue each path, so `{a,b}.c` is the same as `{a.c,b.c}`.

| Query | Output |
|---|---|
| `Fred.{age,h[0]}` | `{"Fred":{"age":50,"hobbies":["bridge"]}}` |
| `{Tim,user_2}.{a,n}` | `{"Tim":{"age":53,"n":null},"user_2":{"name":"bob","a":null}}` |
| `Fred.h.{[0],[2]}` | `{"Fred":{"hobbies":["bridge","chess"]}}` |

- A path can hold any steps, including `**` and other braces.
- What two paths select is combined, so the order of the paths doesn't matter: the result is in document order.
- Don't put spaces after the commas.

### Any depth: `**`

`**` runs the steps after it on the value and on every value inside it, at any depth, and keeps everything they select.

| Query | Output |
|---|---|
| `**.name` | `{"user_1":{"name":"ann"},"user_2":{"name":"bob"}}` |
| `**.h[0]` | `{"Fred":{"hobbies":["bridge"]}}` |
| `Fred.**.age` | `{"Fred":{"age":50}}` |

- `**` needs at least one step after it, and two in a row are invalid.
- A match inside another match is kept whole by the outer one: on `{"a":{"a":1},"b":2}`, `**.a` gives `{"a":{"a":1}}`.

### Leaving keys out: `!`

A `!` straight after a step leaves the keys or indexes it selects out of the result: the object or array it selects from is replaced by what it selects. A single value is put in its place, and several are collected into an array.

| Query | Output |
|---|---|
| `Fred.age!` | `{"Fred":50}` |
| `*.age!` | `{"Tim":53,"Fred":50,"user_1":null,"user_2":null}` |
| `**.name!` | `{"user_1":"ann","user_2":"bob"}` |
| `Fred.h[0]!` | `{"Fred":{"hobbies":"bridge"}}` |
| `Fred.h[1:]!` | `{"Fred":{"hobbies":["yodelling","chess"]}}` |
| `Fred.*!` | `{"Fred":[50,["bridge","yodelling","chess"]]}` |
| `Tim!` | `{"age":53}` |
| `Fred.h![0]` | `{"Fred":["bridge"]}` |

A `!` can follow any step except `**` and `{...}`, anywhere in the query, so it can skip a level you don't need to see. On `examples/simple_object.json`, `**.car!.cou` finds `cards` inside `Lorie.collections` and leaves it out:

```sh
$ jt '**.car!.cou' examples/simple_object.json
{
  "Lorie": {
    "collections": {
      "count": 34
    }
  }
}
```

Inside braces, put the `!` on the steps in each path: `Fred.{age!,h!}`.

## Errors

`jt` prints errors to stderr as `jt: <message>` and exits with status 1. A bad command line, such as a missing query or an unknown option, prints a usage message and exits with status 2.

| Message | Cause | Example |
|---|---|---|
| `invalid query` | the query doesn't parse | `.Tim`, `Tim..age`, `Fred.hobbies.[0]`, `Tim[-1]`, `/(/`, `{Tim,}`, `**`, `a!!`, `{a}!` |
| file or JSON errors | the file is missing, or the input isn't valid JSON | |

A query that parses never fails on the data: steps that don't suit a value select nothing from it.
