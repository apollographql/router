### Configuration YAML errors and `router config upgrade` output read differently ([PR #10848](https://github.com/apollographql/router/pull/10848))

The router uses a new YAML library, so some configuration error messages and the output of `router config upgrade` change:

- A key written twice in the same mapping, such as `listen` under `supergraph`, was reported as `duplicated keys detected in your yaml configuration: 'supergraph.listen'`. It is now reported as `failed to parse yaml: duplicate entry with key "listen"`, which names the repeated key without its parent.
- YAML syntax errors start with `failed to parse yaml` instead of `could not parse yaml`, and the rest of the message is worded differently, for example `did not find expected node content at line 2 column 1, while parsing a flow node`. They still give the line and column.
- `router config upgrade` no longer starts its output with `---`, writes strings such as URLs without quotes, and indents list items level with their parent key instead of two spaces further in.

Configuration that loaded before still loads, so you don't need to change it. If scripts or alerts match the old duplicate-key or syntax error messages, update them to the new wording.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10848
