### Substituted values in Connector `$config` keep their YAML type

A `${env.NAME}` or `${file.PATH}` value in a Connector's `$config` object, or in any other setting that accepts any value, is read as if you had written it directly in YAML: `5` becomes the number `5`, `false` the boolean `false`, an empty value, `~` or `null` becomes null, and `0042` the string `"0042"`. A substitution with other text around it, such as `count-${env.N}`, is a string.

Settings that declare a type still take that type, and a setting that accepts either of two different types, such as a number or a string, keeps the text. OTLP gRPC `metadata` values are still read as strings, so `x-api-key: ${env.API_KEY}` with a numeric key keeps working.

Two smaller changes apply to substituted values:

- A boolean setting such as `enabled: ${env.FLAG}` accepts `True` and `TRUE` as well as `true`, and the same forms of `false`. `yes`, `on` and `1` are still errors.
- An empty or `~` value at an optional number or boolean setting leaves the setting unset. Previously the router refused to start with an error, so if you relied on that error to catch an environment variable that is set but empty, check the variable before starting the router.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10443
