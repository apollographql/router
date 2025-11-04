### fix(response_cache): display cache tags generated from subgraph response in debugger ([PR #8531](https://github.com/apollographql/router/pull/8531))

Get generated cache tags from subgraph response (in `extensions`) when using the debugger.

> For performance reasons. These generated cache tags will only be displayed if the data has been cached in debug mode

By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/8531