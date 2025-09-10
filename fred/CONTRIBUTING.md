# Contributing

This document gives some background on how the library is structured and how to contribute.

## General

* Run rustfmt and clippy before submitting any changes.
    * Formatting should adhere to [rustfmt.toml](./rustfmt.toml), i.e. by running `cargo +nightly fmt --all`
    * VS Code users should use the checked-in settings in the [.vscode](./.vscode) directory
* Please use [conventional commits](https://www.conventionalcommits.org/en/v1.0.0/#summary).

## File Structure

The code has the following structure:

* The [commands](src/commands) folder contains the public interface and private implementation for each of the Redis
  commands, organized by category. This is roughly the same categorization used by
  the [public docs](https://redis.io/commands/). Each of these public command category interfaces are exposed as a trait
  with default implementations for each command.
* The [clients](src/clients) folder contains public client structs that implement and/or override the traits
  from [the command category traits folder](src/commands/impls). The [interfaces](src/interfaces.rs) file contains the
  shared traits required by most of the command category traits, such as `ClientLike`.
* The [monitor](src/monitor) folder contains the implementation of the `MONITOR` command and the parser for the response
  stream.
* The [protocol](src/protocol) folder contains the implementation of the base `Connection` struct and the logic for
  splitting a connection to interact with reader and writer halves in separate tasks.
  The [TLS interface](src/protocol/tls.rs) is also implemented here.
* The [router](src/router) folder contains the logic that implements the sentinel and cluster interfaces. Clients
  interact with this struct via a message passing interface. The interface exposed by the `Router` attempts to hide all
  the complexity associated with sentinel or clustered deployments.
* The [trace](src/trace) folder contains the tracing implementation. Span IDs are manually tracked on each command as
  they move across tasks.
* The [types](src/types) folder contains the type definitions used by the public interface. The Redis interface is
  relatively loosely typed but Rust can support more strongly typed interfaces. The types in this module aim to support
  an optional but more strongly typed interface for callers.
* The [modules](src/modules) folder contains smaller helper interfaces such as a
  lazy [Backchannel](src/modules/backchannel.rs) connection interface and
  the [response type conversion logic](src/modules/response.rs).

See the [design doc](doc/design.md) for more info.

## Examples

### Add A New Command

This example shows how to add `MGET` to the commands.

1. Add the new variant to the `CommandKind` enum, if needed.

    ```rust
    pub enum RedisCommandKind {
      // ...
      Mget,
      // ...
    }

    impl RedisCommandKind {
      // ..

      pub fn to_str_debug(&self) -> &str {
        match *self {
          // ..
          RedisCommandKind::Mget => "MGET",
          // ..
        }
      }

      // ..

      pub fn cmd_str(&self) -> Str {
        match *self {
          // .. 
          RedisCommandKind::Mget => "MGET"
          // ..
        }
      }

      // ..
    }
    ```

2. Create the private function implementing the command in [src/commands/impls/keys.rs](src/commands/impls/keys.rs).

    ```rust
    pub async fn mget<C: ClientLike>(client: &C, keys: MultipleKeys) -> Result<Value, RedisError> {
      // maybe do some kind of validation 
      utils::check_empty_keys(&keys)?;

      let frame = utils::request_response(client, move || {
        // time spent here will show up in traces in the `prepare_command` span
        Ok((RedisCommandKind::Mget, keys.into_values()))
      })
        .await?;

      protocol_utils::frame_to_results(frame)
    }
    ```

   Or use one of the shorthand helper functions or macros.

    ```rust
    pub async fn mget<C: ClientLike>(client: &C, keys: MultipleKeys) -> Result<Value, RedisError> {
      utils::check_empty_keys(&keys)?;
      args_values_cmd(client, keys.into_values()).await
    }
    ```

3. Create the public function in the [src/commands/interfaces/keys.rs](src/commands/interfaces/keys.rs) file.

    ```rust

    // ...

    pub trait KeysInterface: ClientLike {

      // ...

      /// Returns the values of all specified keys. For every key that does not hold a string value or does not exist, the special value nil is returned.
      ///
      /// <https://redis.io/commands/mget>
      fn mget<R, K>(&self, keys: K) -> impl Future<Output=RedisResult<R>> + Send
      where
        R: FromRedis,
        K: Into<MultipleKeys> + Send,
      {
        async move {
          into!(keys);
          commands::keys::mget(self, keys).await?.convert()
        }
      }

      // ...
    }
    ```

4. Implement the interface on the client structs, if needed.

   In the [Client](src/clients/redis.rs) file.

    ```rust
    impl KeysInterface for Client {}
    ```

   In the [transaction](src/clients/transaction.rs) file.

    ```rust
    impl KeysInterface for Transaction {}
    ```

## Adding Tests

Integration tests are in the [tests/integration](tests/integration) folder organized by category. See the
tests [README](tests/README.md) for more information.

Using `MGET` as an example again:

1. Write tests in the [keys](tests/integration/keys/mod.rs) file.

    ```rust
    pub async fn should_mget_values(client: Client, _: Config) -> Result<(), RedisError> {
      let expected: Vec<(&str, Value)> = vec![("a{1}", 1.into()), ("b{1}", 2.into()), ("c{1}", 3.into())];
      for (key, value) in expected.iter() {
        let _: () = client.set(key, value.clone(), None, None, false).await?;
      }
      let values: Vec<i64> = client.mget(vec!["a{1}", "b{1}", "c{1}"]).await?;
      assert_eq!(values, vec![1, 2, 3]);

      Ok(())
    }
    ```

2. Call the tests from the [centralized server tests](tests/integration/centralized.rs).

    ```rust
    mod keys {
      // ..
      centralized_test!(keys, should_mget_values);
    }

    ```

3. Call the tests from the [cluster server tests](tests/integration/clustered.rs).

    ```rust
    mod keys {
      // ..
      cluster_test!(keys, should_mget_values);
    }
    ```

These macros will generate test wrapper functions to call your test 4 times based on the following options:

* Clustered vs centralized deployments
* RESP2 vs RESP3 protocol modes
