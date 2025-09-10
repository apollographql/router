# Alternative Runtimes

`fred` was originally written for Tokio runtimes, but callers can enable experimental support for other runtimes via the
`glommio`, `monoio` and `smol` features. Only one runtime interface feature can be enabled at a time.

These runtimes have some important differences in their scheduling interfaces based on whether they implement a
work-stealing or thread-per-core scheduling model.

* [tokio::spawn](https://docs.rs/tokio/latest/tokio/task/fn.spawn.html)
  and [async_task::spawn](https://docs.rs/async-task/latest/async_task/fn.spawn.html) require a `Send` bound on the
  spawned future so that the scheduler can implement work-stealing across threads.
* Glommio and Monoio both use a thread-per-core model that works best when threads do not need to share or synchronize
  any state. Both the [spawn_local](https://docs.rs/glommio/latest/glommio/fn.spawn_local.html)
  and [spawn_local_into](https://docs.rs/glommio/latest/glommio/fn.spawn_local_into.html) functions
  spawn tasks on the same thread and therefore do not have a `Send` bound.

`fred` was originally written with message-passing design patterns targeting a Tokio runtime and therefore the `Send`
bound from `tokio::spawn` leaked into all the public interfaces that send messages across tasks. This includes nearly
all the public command traits, including the base `ClientLike` trait.

If any of the alternative runtime features are enabled the client's interface and internal implementation will change in
several ways based on the runtime scheduling model. For thread-per-core runtimes (`glommio` and `monoio`)
this includes:

* The `Send + Sync` bounds will be removed from all generic input parameters, where clause predicates, and `impl Trait`
  return types.
* Internal `Arc` usages will change to `Rc`.
* Internal `RwLock` and `Mutex` usages will change to `RefCell`.
* Internal usages of `std::sync::atomic` types will change to thin wrappers around a `RefCell`.
* Any Tokio message passing interfaces (`BroadcastReceiver`, etc) will change to the closest equivalent provided by the
  runtime.

The public docs
on [docs.rs](https://docs.rs/fred/latest) will continue to show the Tokio interfaces that require `Send` bounds, but
callers can find the latest rustdocs for both runtimes on the
`gh-pages` branch:

[Glommio Documentation](https://aembke.github.io/fred.rs/glommio/fred/index.html)

[Tokio Documentation](https://aembke.github.io/fred.rs/tokio/fred/index.html)

## Compatibility

# Glommio

See the [Glommio Introduction](https://www.datadoghq.com/blog/engineering/introducing-glommio/) for more info.

When building with `--features glommio` a Tokio compatability layer will be used to map between the two runtime's
versions of `AsyncRead` and `AsyncWrite`. This enables the existing codec interface (`Encoder` + `Decoder`) to work with
Glommio's network types. As a result, for now some Tokio dependencies are still required when using Glommio features.

This approach also allows the `tokio-native-tls` and `tokio-rustls` modules to work with Glommio's network types.

[Glommio Example](https://github.com/aembke/fred.rs/blob/main/examples/glommio.rs)

# Monoio

Work In Progress
