### Add config schema JSON as a build artifact in GitHub releases ([PR #8594](https://github.com/apollographql/router/pull/8594))

The router configuration schema JSON file is now automatically generated and included as a build artifact in GitHub releases. This allows users to reference the schema for a specific version via URLs like `https://github.com/apollographql/router/releases/download/v{VERSION}/config-schema.json`.

The schema is generated using the same `router config schema` command that users can run locally, ensuring consistency between the released schema and what users see when running the command.

By [@shanemyrick](https://github.com/shanemyrick) in https://github.com/apollographql/router/pull/8594

