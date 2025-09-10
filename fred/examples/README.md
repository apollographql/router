Examples
========

* [Basic](./basic.rs) - Basic client usage.
* [Axum](./axum.rs) - Use a client pool with [Axum](https://crates.io/crates/axum).
* [Actix](./actix.rs) - Use a client pool with [Actix](https://github.com/actix/actix-web).
* [TLS](./tls.rs) - Setting up a client that uses TLS.
* [Publish-Subscribe](./pubsub.rs) - Use multiple clients together with the pubsub interface in a way that survives
  network interruptions.
* [Blocking](./blocking.rs) - Use multiple clients with the blocking list interface.
* [Transactions](./transactions.rs) - Use the MULTI/EXEC interface on a client.
* [Pipeline](./pipeline.rs) - Use the manual pipeline interface.
* [Streams](./streams.rs) - Use `XADD` and `XREAD` to communicate between tasks.
* [Lua](./lua.rs) - Use the Lua scripting interface on a client.
* [Scan](./scan.rs) - Use the SCAN interface to scan and read keys.
* [Pool](./pool.rs) - Use a round-robin client pool.
* [Dynamic Pool](./dynamic_pool.rs) - Use a client pool that scales dynamically.
* [Monitor](./monitor.rs) - Process a `MONITOR` stream.
* [Sentinel](./sentinel.rs) - Connect using a sentinel deployment.
* [Serde JSON](./serde_json.rs) - Use the `serde-json` feature to convert between Redis types and JSON.
* [Redis JSON](./redis_json.rs) - Use the `i-redis-json` feature with `serde-json` types.
* [Custom](./custom.rs) - Send custom commands or operate on RESP frames.
* [DNS](./dns.rs) - Customize the DNS resolution logic.
* [Client Tracking](./client_tracking.rs) -
  Implement [client side caching](https://redis.io/docs/manual/client-side-caching/).
* [Events](./events.rs) - Respond to connection events with the `EventsInterface`.
* [Keyspace Notifications](./keyspace.rs) - Use
  the [keyspace notifications](https://redis.io/docs/manual/keyspace-notifications/) interface.
* [Misc](./misc.rs) - Miscellaneous or advanced features.
* [Replicas](./replicas.rs) - Interact with cluster replica nodes via a `RedisPool`.
* [Glommio](./glommio.rs) - Use the [Glommio](https://github.com/DataDog/glommio) runtime.
  See [the source docs](../src/runtime/glommio/README.md) for more information.

Or see the [tests](../tests/integration) for more examples.