### Remove dead parsing code based on apollo-parser AST ([Issue #2636](https://github.com/apollographql/router/issues/2636))

Now that https://github.com/apollographql/router/pull/2466 has been in released version of apollo-router for long enough, remove now-unused previous version of parsing code that was based on apollo-parser’s AST instead of apollo-compiler’s HIR.

This will unlock further refactoring in https://github.com/apollographql/router/issues/2483.

Fixes https://github.com/apollographql/router/issues/2636

By [@SimonSapin](https://github.com/SimonSapin) in https://github.com/apollographql/router/pull/2637
