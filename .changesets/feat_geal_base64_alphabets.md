### Rhai: support alternative base64 alphabets ([Issue #3783](https://github.com/apollographql/router/issues/3783))

This adds support for alternative base64 alphabets:
* STANDARD
* STANDARD_NO_PAD
* URL_SAFE
* URL_SAFE_NO_PAD

They can be used as follows:

```
let original = "alice and bob";
let encoded = base64::encode(original, base64::URL_SAFE);
// encoded will be "YWxpY2UgYW5kIGJvYgo="
try {
    let and_back = base64::decode(encoded, base64::URL_SAFE);
    // and_back will be "alice and bob"
}
```

The default when the alphabet argument is not specified is STANDARD.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3885