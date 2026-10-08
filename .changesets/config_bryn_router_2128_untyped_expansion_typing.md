### Substituted values in Connector `$config` keep their YAML type

A `${env.NAME}` or `${file.PATH}` value in a Connector's `$config` object is read as YAML: `5` becomes the number `5`, `false` becomes `false`, an empty value, `~` or `null` becomes `null`, and `0042` stays the string `"0042"`. This applies to any setting that accepts any value. A substitution with text around it, such as `count-${env.N}`, is a string.

Other substituted values work like this:

- A setting with a declared type takes that type. A setting that accepts either a number or a string keeps the text.
- OTLP gRPC `metadata` values are strings, so `x-api-key: ${env.API_KEY}` works with a numeric key.
- A boolean setting accepts `true`, `True` and `TRUE`, and the same forms of `false`. `yes`, `on` and `1` are errors.
- An empty or `~` value at an optional number or boolean setting leaves the setting unset.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10443
