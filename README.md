# Examples

## Sample

```json

{
  "Tim": { "age": 53 },
  "Fred": {
    "age": 50, 
    "hobbies": [
      "bridge", 
      "yodelling"
    ]
}
}


### All Keys

```
.*^
```

yields:

```
["Tim","Fred"]
```
```


### All Ages

```
.*.age
```

yields:

```json
[53, 50]
```
```

### Nested objects

```
.*
```

yields:

```json
[
  {"age": 53},
  {
    "age": 50, 
    "hobbies": [
      "bridge", 
      "yodelling"
    ]
  }
]
```
```


### Nested Tim Object

```
.Tim
```

yields:

```json
{"age": 53}
```
```

### Tim's age

```
.Tim.age
```

yields:

```
53
```
```


### hobbies

.*.hobbies

yields:

```json
[
  null,
  ["bridge", "yodelling"]
]
```
```
```
