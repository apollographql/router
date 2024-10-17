### Add ability to configure DNS resolution strategy ([PR #6109](https://github.com/apollographql/router/pull/6109))

The router now supports choosing a DNS resolution strategy for the coprocessor's and subgraph's URLs.
The new option is called `dns_resolution_strategy` and supports the following values:
* `ipv4_only` - Only query for `A` (IPv4) records.
* `ipv6_only` - Only query for `AAAA` (IPv6) records.
* `ipv4_and_ipv6` - Query for both `A` (IPv4) and `AAAA` (IPv6) records in parallel.
* `ipv6_then_ipv4` - Query for `AAAA` (IPv6) records first; if that fails, query for `A` (IPv4) records.
* `ipv4_then_ipv6`(default) - Query for `A` (IPv4) records first; if that fails, query for `AAAA` (IPv6) records.

To change the DNS resolution strategy applied to the subgraph's URL:
```yaml title="router.yaml"
traffic_shaping:
  all:
    dns_resolution_strategy: ipv4_then_ipv6

```

You can also change the DNS resolution strategy applied to the coprocessor's URL:
```yaml title="router.yaml"
coprocessor:
  url: http://coprocessor.example.com:8081
  client:
    dns_resolution_strategy: ipv4_then_ipv6

```

By [@IvanGoncharov](https://github.com/IvanGoncharov) in https://github.com/apollographql/router/pull/6109
