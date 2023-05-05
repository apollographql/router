### Fix panic when compressing small responses with gzip 

1.17.0 has a regression where compressing small responses would trigger invalid buffer management, and the router would panic.

By [@dbanty](https://github.com/dbanty) in https://github.com/apollographql/router/pull/3047