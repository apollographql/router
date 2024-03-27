### Remove invalid payload on graphql-ws Ping message ([Issue #4852](https://github.com/apollographql/router/issues/4852))

According to [graphql-ws spec](https://github.com/enisdenjo/graphql-ws/blob/master/PROTOCOL.md#ping) `Ping` payload should be an object or null but router was sending a string.
To ensure better compatibility Ping's payload was removed. 

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/4852