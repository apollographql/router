### Add configuration option to limit maximum batch size ([PR #7005](https://github.com/apollographql/router/pull/7005))

Add an optional `maximum_size` parameter to the batching configuration.

* When specified, the router will reject requests which contain more than `maximum_size` queries in the client batch.
* When unspecified, the router performs no size checking (the current behavior).

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/7005
