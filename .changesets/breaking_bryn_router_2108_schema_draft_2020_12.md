### The configuration JSON Schema is now draft 2020-12

`router config schema` now prints a JSON Schema that declares draft 2020-12 (`"$schema": "https://json-schema.org/draft/2020-12/schema"`) instead of draft 7. Its definitions move from `definitions` to `$defs`, so references read `#/$defs/Name`. A description or default now sits beside its `$ref` instead of inside a one-item `allOf`. The configuration the router accepts doesn't change, and neither do its error messages.

What to do:

- **Editors:** regenerate any saved schema with `router config schema`. VS Code with the Red Hat YAML extension 1.20.0 or later reads draft 2020-12, and JetBrains IDEs should read it from 2024.2. Older editors still validate against the schema but may not show descriptions on hover. If you set a schema version by hand in a JetBrains IDE, choose 2020-12.
- **Scripts** that read the schema's `definitions`, for example `jq '.definitions'`, should read `$defs` instead.
- **Native plugins** with a hand-written `JsonSchema` implementation must build references with the generator, for example `generator.subschema_for::<T>()`, rather than writing `#/definitions/...` by hand. A hard-coded `#/definitions/` reference no longer resolves, and the router fails to start.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10517
