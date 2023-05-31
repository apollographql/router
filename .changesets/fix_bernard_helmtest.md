### Helm: Running of `helm test` no longer fails

Running `helm test` was generating an error since `wget` was sending a request without a proper body and expecting an HTTP status response of 2xx.   Without the proper body, it expectedly resulted in an HTTP status of 400.  By switching to using `netcat` (or `nc`) we will now check that the port is up and use that to determine that the router is functional.

By [@bbardawilwiser](https://github.com/bbardawilwiser) in https://github.com/apollographql/router/pull/3096
