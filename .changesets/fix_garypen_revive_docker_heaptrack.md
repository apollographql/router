### Make 'router' user the owner of the docker image's /dist/data directory ([PR #4898](https://github.com/apollographql/router/pull/4898))

Since we made our images more secure, we run our router process as user 'router'. If we are running under 'heaptrack', e.g.: in a debug image, then we cannot write to /dist/data because it is owned by 'root'.

This changes the ownership of /dist/data from 'root' to 'router' to allow writes to succeed.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4898