### Nested authorization and coprocessor settings reject unknown keys

`authorization.directives`, `authorization.directives.errors` and the coprocessor `router`, `supergraph`, `execution` and `connector` stage blocks used to ignore keys they didn't define. They now reject them, the same as the rest of their plugin sections, so a typo such as `coprocessor.router.reqest` fails when the configuration loads instead of the stage silently doing nothing. The configuration JSON Schema marks these blocks `additionalProperties: false`.

To migrate, run `router config validate` and correct or remove any unknown key it reports in these blocks.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/####
