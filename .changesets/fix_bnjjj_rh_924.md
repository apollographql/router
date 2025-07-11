### Fix the subscription duplication problem when the client terminates the original subscription. ([PR #7879](https://github.com/apollographql/router/pull/7879))

Address a regression in subscription deduplication introduced in #5505. The client connection close event does not need handling in this task because it is managed elsewhere. Additionally, part of #5505 resolved the issue related to closing the websocket connection. This PR also includes tests to ensure no future regressions related to subscription deduplication and websocket close events.

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/7879