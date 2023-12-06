### Use both Rust and JS Validation as default([issue#4159](https://github.com/apollographql/router/issues/4159))

As part of the process to replace JavaScript validation with a more performant Rust validation in the Router, we are enabling the Router to run both validations as a default. This allows us to definitively assess reliability and stability of Rust validation before completely removing JavaScript validation. As before, it's possible to toggle between implementations using `experimental_graphql_validation_mode` config key. Possible values are: `new` (runs only Rust-based validation), `legacy` (runs only JS-based validation), `both` (runs both in comparison, logging errors if a difference arises).


By [@lrlna](https://github.com/lrlna) in https://github.com/apollographql/router/pull/4161