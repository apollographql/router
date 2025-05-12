### Fix Multiline URL Connectors Composition ([PR #7470](https://github.com/apollographql/router/pull/7470))

Multiline absolute URLs were failing to compose connectors url, this MR removes URL String Template internal whitespace characters like `\n`, `\u` and ` `.

By [@naomijub](https://github.com/naomijub) in https://github.com/apollographql/router/pull/7470
