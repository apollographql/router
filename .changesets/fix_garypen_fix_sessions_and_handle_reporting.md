### Fix session counting and the reporting of file handle shortage ([PR #5834](https://github.com/apollographql/router/pull/5834))

Session counting incorrectly included connections to the health check or other non-graphql connections. This is now corrected so that only connections to the main graphql port are counted.

Warnings about file handle shortages are now handled correctly as a global resource.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5834