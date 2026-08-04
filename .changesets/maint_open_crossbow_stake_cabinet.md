### Delete `feature_discussions.json` and all references ([PR #9893](https://github.com/apollographql/router/pull/9893))

Removes `feature_discussions.json` because all other experimentals ship without discussion URLs. This involved also removing related traits and test files as well as the startup code that logged these discussion urls.

By [@conwuegb](https://github.com/conwuegb) in https://github.com/apollographql/router/pull/9893