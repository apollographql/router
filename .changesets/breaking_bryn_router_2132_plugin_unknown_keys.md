### More plugin configuration sections reject unknown keys

The `authorization`, `fleet_detector`, `enhanced_client_awareness` and `progressive_override` sections used to ignore keys they didn't define. They now reject them, the same as every other plugin section, so a typo such as `authorization.require_authentcation` fails when the configuration loads instead of being silently dropped. The configuration JSON Schema marks these sections `additionalProperties: false`. Empty sections such as `fleet_detector: {}` are still valid.

To migrate, run `router config validate` and correct or remove any unknown key it reports in these sections.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10305
