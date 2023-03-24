### keep errors set on `/_entities` ([Issue #2731](https://github.com/apollographql/router/issues/2731))

Some subgraphs do not set errors per entities but on the entire path. We should still transmit them.

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2756