### Update the query planner to 2.4.6 ([Issue #3133](https://github.com/apollographql/router/issues/3133))

This fixes some errors in query planning on fragment with overlapping subselections, with a message of the form "Cannot add selection of field X to selection set of parent type Y".

The new router-bridge version also allows updating some dependencies that were fixed to older versions: bytes, regex, once_cell, tokio, uuid

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/3135