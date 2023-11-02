### Use trust dns for hyper client resolver ([Issue #4030](https://github.com/apollographql/router/issues/4030))

Investigating memory revealed that the default hyper client DNS resolver had a negative impact on the memory footprint of the router.

It may also not be respecting TTL correctly. Let's replace the default with Trust DNS.

fixes: #4030

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4088