### Fix private information caching in entity cache ([PR #5599](https://github.com/apollographql/router/pull/5599))

The router previously had an issue where private data could be stored at the wrong key, resulting in the data not appearing to be cached. This has been fixed by updating the cache key with the private data. 

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/5599