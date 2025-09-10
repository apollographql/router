## 10.1.0

* Add `DynamicPool` interface
* Add `actix-web` example
* Add `tcp-user-timeouts` feature flag

## 10.0.4

* Fix unresponsive checks with blocked connections
* Fix potential hanging calls to `quit` when called concurrently by multiple tasks

## 10.0.3

* Support SCAN functions in mocks

## 10.0.2

* Fix intermittent transaction timeouts

## 10.0.1

* Fix reconnection errors when no connections exist

## 10.0.0

* Reduced memory footprint and significant write throughput improvements
* Rename common interfaces to remove `Redis` prefixes
* Add `WITHSCORES` to `ZRANK` and `ZREVRANK`
* Add `GT|LT|NX|XX` options to `EXPIRE` and `EXPIREAT`
* Add `scan_page` interface
* Add optional message to `PING`
* Remove deprecated or redundant config options
* Refactor public types into submodules
* Add `i-hexpire` feature flag
* Support async blocks in `on_*` event handler functions
* Add an interface to cancel scanning functions
* Update `rustls-native-certs` to 0.8
* Support `valkey://` scheme in `Config::from_url`.
* Support combining `Options` and `Replicas` clients

### Upgrading from 9.x

This update contains some significant performance improvements in the form of reduced Tokio scheduling overhead and
reduced memory usage during the frame encoding process. It also contains several cosmetic changes designed to support
future scenarios where Valkey and Redis start to diverge from one another.

### Notable Breaking Changes

The compiler should guide callers through most of these changes.

* The `auto_pipeline` config option was removed. All clients now automatically pipeline commands across Tokio tasks.
* The `BackpressureConfig` struct was removed. Callers should use `max_command_buffer_len` instead.
* The `HEXPIRE`, `HTTL`, etc, commands are now gated by an `i-hexpire` feature flag. Note that this requires Redis >
  =7.2.5.
* Some potentially redundant `ReplicaConfig` fields were removed. The client now uses the associated `ReconnectPolicy`
  fields instead, where applicable.
* The `types` module was becoming too large and needed refactoring. Many types were moved to submodules, which likely
  requires changes to certain import statements.
* Many of the common public types were renamed to remove the `Redis` prefix, such as `RedisConfig`, `RedisClient`,
  `RedisPool`, etc.
* `rustls-native-certs` was upgraded to 8.x.
* The `specialize-into-bytes` feature flag was removed. This is now the default behavior.
* The `on_error` and `error_rx` event handler interface now contains an optional server identifier.

### Behavior Changes

* In the past `fred` spawned a separate task per connection in order to read from all sockets concurrently. In 10.0.0
  each client reads and writes to all connections in a single task.
* Write throughput is improved by a factor of 3-5x depending on the use case.
* All transactions are now pipelined automatically.

## 9.4.0

* Change scanning functions to automatically continue when the current page is dropped
* Add `scan_buffered` and `scan_cluster_buffered` interfaces
* Add `specialize-into-bytes` feature flag

## 9.3.0

* Add `SETNX`, `ECHO`, `TYPE`, `EXPIRETIME`, and `PEXPIRETIME` commands
* Add hashmap expiration commands (`HTTL`, `HEXPIRE`, etc)
* Change `active_connections` to preempt reconnections

## 9.2.1

* Fix docs.rs documentation features

## 9.2.0

* Add initial support for the [Glommio](https://github.com/DataDog/glommio) runtime
* Add `credential-provider` feature
* Fix pipeline processing in mocks
* Support pipelined transactions

## 9.1.2

* Fix `FT.AGGREGATE` command with `SORTBY` operation

## 9.1.1

* Fix tracing span names and missing fields

## 9.1.0

* Add [RediSearch](https://github.com/RediSearch/RediSearch) interface.
* Adapt testing and CI processes to test Redis and Valkey
* Add `FromIterator` impl to `RedisMap`
* Add `ExclusivePool` client
* Support `redis+unix` config URLs for Unix domain sockets
* Add `PEXPIRE` and `PEXPIREAT`
* Replace `trust-dns-resolver` with `hickory-resolver`

## 9.0.3

* Fix `bytes_utils` min version
* Fix rustls reexports with `enable-rustls-ring`

## 9.0.2

* Add `enable-rustls-ring` feature flag

## 9.0.1

* Fix `partial-tracing` imports

## 9.0.0

This version should reduce compilation times for most use cases.

* **RPITIT / AFIT**
* Set MSRV to 1.75
* Upgrade `rustls` to 0.23
* Upgrade `redis-protocol` to 5.0.0
* Split public interfaces with new feature flags.
* Add `ClusterDiscoveryPolicy` configuration options.
* Add `SORT` and `SORT_RO`
* Add `cluster_hash` policy to `Options`
* Change tracing span names to
  follow [OpenTelemetry naming conventions](https://opentelemetry.io/docs/specs/semconv/general/attribute-naming/).

### Upgrading from 8.x

* Callers that use certain managed services or Kubernetes-based deployment models should review the
  new `ClusterDiscoveryPolicy`.
* Double-check the new feature flags. The `codec` feature was also moved
  to [redis-protocol](https://github.com/aembke/redis-protocol.rs).
* Rustls - Check the new [aws-lc-rs](https://aws.github.io/aws-lc-rs/requirements/index.html) requirements or switch
  back to `rustls/ring`.
* Note the new [tracing span names](src/trace/README.md).

## 8.0.6

* Add `TransactionInterface` to `RedisPool`

## 8.0.5

* Add conversion from `HashMap` to `MultipleOrderedPairs`.
* Remove `lazy_static`
* Update examples

## 8.0.4

* Fix tracing span annotations.

## 8.0.3

* Box large futures to reduce stack usage.

## 8.0.2

* Fix cluster replica failover at high concurrency.
* Fix potential race condition initializing the mocking layer.

## 8.0.1

* Add a shorthand `init` interface.
* Fix cluster replica failover with unresponsive connections.
* Fix RESP3 connection init when used without a password.

## 8.0.0

* Remove the `globals` interface.
* Support unix domain sockets.
* Add a Redis TimeSeries interface.
* Improve unresponsive connection checks.
* Move several feature flags to configuration options.
* Add a [benchmarking](bin/benchmark) tool.
* Update to Rustls 0.22.
* Add several new connection configuration options.
* Add a `fail_fast` flag to commands.
* Switch to [crossbeam types](https://crates.io/crates/crossbeam-queue) internally.

### Upgrading from 7.x

Using `..Default::default()` with the various configuration structs can avoid most of the breaking changes here.

Notable changes:

* Several configuration options were moved from `globals` to `ConnectionConfig` and `PerformanceConfig`.
* Several feature flags were moved to configuration fields, including:
    * `ignore-auth-error`
    * `pool-prefer-active`
    * `reconnect-on-auth-error`
    * `auto-client-setname`
* The `on_message` and `on_keyspace_event` functions were renamed and moved to the `EventInterface`. They now use the
  same naming conventions as the other event streams.

## 7.1.2

* Fix intermittent cluster routing errors

## 7.1.1

* Fix cluster failover in transactions

## 7.1.0

* Fix panic when reconnect delay jitter is 0
* Support percent encoding in URLs
* Support tuples for `RedisValue` and `MultipleKeys`
* Make `CLIENT ID` checks optional
* Update dependencies

## 7.0.0

* Added a new client [builder](src/types/builder.rs) and configuration interface.
* Reworked or removed the majority of the `globals` interface.
*
* Support multiple IP addresses in the `Resolve` interface.
* Add `with_options` command configuration interface.
* Replaced the `no-client-setname` feature flag with `auto-client-setname`.
* Add an interface to configure TCP socket options.
* Removed the automatic `serde_json::Value` -> `RedisValue` type conversion logic.
    * This unintentionally introduced some ambiguity on certain interfaces.
    * The `RedisValue` -> `serde_json::Value` type conversion logic was not changed.
* Reworked the majority of the `RedisPool` interface.
* Moved and refactored the `on_*` functions into a new `EventInterface`.
* Fixed bugs with the `Replica` routing implementation.
* Fixed bugs related to parsing single-element arrays.
* Changed several `FromRedis` type conversion rules. See below or the `FromRedis` docs for more information.
* Add a [RedisJSON](https://github.com/RedisJSON/RedisJSON/) interface.
* Add a RESP2 and RESP3 codec interface.

### Upgrading from 6.x

Notable interface changes:

* `ArcStr` has been replaced with `bytes_utils::Str`.
* Timeout arguments or fields now all use `std::time::Duration`.
* Many of the old global or performance config values can now be set on individual commands via the `with_options`
  interface.
* The `RedisPool` interface now directly implements `ClientLike` rather than relying on `Deref` shenanigans.
* The `on_*` event functions were moved and renamed. Reconnection events now include the associated `Server`.
* The `tls_server_name` field on `Server` is now properly gated by the TLS feature flags.
* Mocks are now optional even when the feature flag is enabled.

Notable implementation Changes:

* `Pipeline` and `Transaction` structs can now be reused. Calling `exec`, `all`, `last`, or `try_all` no longer drains
  the inner command buffer.
* Many of the default timeout values have been lowered significantly, often from 60 sec to 10 sec.
* In earlier versions the `FromRedis` trait implemented a few inconsistent or ambiguous type conversions policies.
    * Most of these were consolidated under the `default-nil-types` feature flag.
    * It is recommended that callers review the updated `FromRedis` docs or see the unit tests
      in [responses](src/modules/response.rs).
* The `connect` function can now be called more than once to force reset all client state.

## 6.3.2

* Fix a bug with connection errors unexpectedly ending the connection task.

## 6.3.1

* Update various dependencies
* Move `pretty-env-logger` to `dev-dependencies`
* Update rustfmt.toml

## 6.3.0

* Fix cluster replica discovery with Elasticache
* Fix cluster replica `READONLY` usage
* Fix compilation error with `full-tracing`
* Support `Vec<(T1, T2, ...)>` with `FromRedis`

## 6.2.1

* Fix cluster failover with paused nodes

## 6.2.0

* Add `Pipeline::try_all`
* Add missing pubsub commands
* Improve docs, examples

## 6.1.0

* Add a [client tracking](https://redis.io/docs/manual/client-side-caching/) interface.
* Add a global config value for broadcast channel capacity.
* Add an interface to interact with individual cluster nodes.
* Fix missing `impl StreamInterface for Transaction`
* Add all `RedisClient` command traits to `SubscriberClient`

## 6.0.0

* Refactored the connection and protocol layer.
* Add a manual `Pipeline` interface for pipelining commands within a task.
* Add a `Replica` client for interacting with replica nodes.
* Rework the `Transaction` interface to buffer commands in memory before EXEC/DISCARD.
* Rework the cluster discovery and failover implementation.
* Rework the MOVED/ASK implementation to more quickly and reliably follow cluster redirects.
* Rework the sentinel interface to more reliably handle failover scenarios.
* Fix several bugs related to detecting closed connections.
* Support the `functions` interface.
* Add `Script`, `Library`, and `Function` structs.
* Add `Message` and `MessageKind` pubsub structs.
* Add a DNS configuration interface.
* Rework the `native-tls` interface to support fully customizable TLS configurations.
* Add `rustls` support.
    * Note: both TLS feature flags can be used at the same time.
* Add a mocking layer interface.

### Updating from 5.x

Potentially breaking changes in 6.x:

* Switched from `(String, u16)` tuples to the `Server` struct for all server identifiers.
* New TLS feature flags: `enable-rustls` and `enable-native-tls`.
    * `vendored-tls` is now `vendored-openssl`
* New TLS configuration process: see the [example](examples/tls.rs).
* New [transaction](examples/transactions.rs) interface.
    * `Transaction` commands are now buffered in memory before calling `exec()` or `discard()`.
* New backpressure configuration options, most notably the `Drain` policy. This is now the default.
* Changed the type and fields on `BackpressurePolicy::Sleep`.
* New [custom command interface](examples/custom.rs) for managing cluster hash slots.
* Removed or renamed some fields on `RedisConfig`.
* Changed the pubsub receiver interface to use `Message` instead of `(String, RedisValue)` tuples.
* Changed the `on_*` family of functions to return
  a [BroadcastReceiver](https://docs.rs/tokio/latest/tokio/sync/broadcast/struct.Receiver.html).
* The `FromRedis` trait converts `RedisValue::Null` to `"nil"` with `String` and `Str`.

## 5.2.0

* Reduce number of `tokio` features
* Use 6379 as default cluster port in `from_url`
* Fix RESP3 auth error (https://github.com/aembke/fred.rs/issues/54)

## 5.2.0

* Reduce number of `tokio` features
* Use 6379 as default cluster port in `from_url`
* Fix RESP3 auth error (https://github.com/aembke/fred.rs/issues/54)

## 5.2.0

* Reduce number of `tokio` features
* Use 6379 as default cluster port in `from_url`
* Fix RESP3 auth error (https://github.com/aembke/fred.rs/issues/54)

## 5.1.0

* Update `redis-protocol` and `nom`
* Add `no-client-setname` feature flag

## 5.0.0

* Bug fixes
* Support URL parsing into a `RedisConfig`
* Update `bzpopmin` and `bzpopmax` return type
* Remove unimplemented `mocks` feature

## 5.0.0-beta.1

* Rewrite the [protocol parser](https://github.com/aembke/redis-protocol.rs), so it can decode frames without moving or
  copying the underlying bytes
* Change most command implementations to avoid unnecessary allocations when using static str slices
* Rewrite the public interface to use different traits for different parts of the redis interface
* Relax some restrictions on certain commands being used in a transaction
* Implement the Streams interface (XADD, XREAD, etc.)
* RESP3 support
* Move most perf configuration options from `globals` to client-specific config structs
* Add backpressure configuration options to the client config struct
* Fix bugs that can occur when using non-UTF8 byte arrays as keys
* Add the `serde-json` feature
* Handle more complicated failure modes with Redis clusters
* Add a more robust and specialized pubsub subscriber client
* Ergonomics improvements on the public interfaces
* Improve docs
* More tests

## 4.3.2

* Fix https://github.com/aembke/fred.rs/issues/27
* Fix https://github.com/aembke/fred.rs/issues/26

## 4.3.1

* Fix authentication bug with `sentinel-auth` tests
* Update tests and CI config for `sentinel-auth` feature
* Add more testing scripts, update docs
* Switch to CircleCI

## 4.3.0

* Add `sentinel-auth` feature

## 4.2.3

* Add `NotFound` error kind variant
* Use `NotFound` errors when casting `nil` server responses to non-nullable types

## 4.2.2

* Remove some unnecessary async locks
* Fix client pool `wait_for_connect` implementation

## 4.2.1

* Fix https://github.com/aembke/fred.rs/issues/11

## 4.2.0

* Support Sentinel clients
* Fix broken doc links

## 4.1.0

* Support Redis Sentinel
* Sentinel tests
* Move metrics behind compiler flag

## 4.0.0

* Add generic response interface.
* Add tests

## 3.0.0

See below.

## 3.0.0-beta.4

* Add support for the `MONITOR` command.

## 3.0.0-beta.3

* Redo cluster state change implementation to diff `CLUSTER NODES` changes
* MOVED/ASK errors no longer initiate reconnection logic
* Fix chaos monkey tests

## 3.0.0-beta.2

* Extend and refactor RedisConfig options
* Change RedisKey to work with bytes, not str
* Support unblocking clients with a control connection
* First draft of chaos monkey tests
* Custom reconnect errors feature

## 3.0.0-beta.1

* Rewrite to use async/await
* Add Lua support
* Add transaction support
* Add hyperloglog, geo, acl, memory, slowlog, and cluster command support
* Add tests
* Add [pipeline_test](bin/pipeline_test) application

## < 3.0.0

See the old repository at [azuqua/fred.rs](https://github.com/azuqua/fred.rs).