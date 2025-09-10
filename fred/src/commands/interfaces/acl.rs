use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  types::{FromValue, MultipleStrings, MultipleValues, Value},
};
use bytes_utils::Str;
use fred_macros::rm_send_if;
use futures::Future;

/// Functions that implement the [ACL](https://redis.io/commandserver) interface.
#[rm_send_if(feature = "glommio")]
pub trait AclInterface: ClientLike + Sized {
  /// Create an ACL user with the specified rules or modify the rules of an existing user.
  ///
  /// <https://redis.io/commands/acl-setuser>
  fn acl_setuser<S, V>(&self, username: S, rules: V) -> impl Future<Output = FredResult<()>> + Send
  where
    S: Into<Str> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(username);
      try_into!(rules);
      commands::acl::acl_setuser(self, username, rules).await
    }
  }

  /// When Redis is configured to use an ACL file (with the aclfile configuration option), this command will reload
  /// the ACLs from the file, replacing all the current ACL rules with the ones defined in the file.
  ///
  /// <https://redis.io/commands/acl-load>
  fn acl_load(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::acl::acl_load(self).await }
  }

  /// When Redis is configured to use an ACL file (with the aclfile configuration option), this command will save the
  /// currently defined ACLs from the server memory to the ACL file.
  ///
  /// <https://redis.io/commands/acl-save>
  fn acl_save(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::acl::acl_save(self).await }
  }

  /// The command shows the currently active ACL rules in the Redis server.
  ///
  /// <https://redis.io/commands/acl-list>\
  fn acl_list<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::acl::acl_list(self).await?.convert() }
  }

  /// The command shows a list of all the usernames of the currently configured users in the Redis ACL system.
  ///
  /// <https://redis.io/commands/acl-users>
  fn acl_users<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::acl::acl_users(self).await?.convert() }
  }

  /// The command returns all the rules defined for an existing ACL user.
  ///
  /// <https://redis.io/commands/acl-getuser>
  fn acl_getuser<R, U>(&self, username: U) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    U: TryInto<Value> + Send,
    U::Error: Into<Error> + Send,
  {
    async move {
      try_into!(username);
      commands::acl::acl_getuser(self, username).await?.convert()
    }
  }

  /// Delete all the specified ACL users and terminate all the connections that are authenticated with such users.
  ///
  /// <https://redis.io/commands/acl-deluser>
  fn acl_deluser<R, S>(&self, usernames: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(usernames);
      commands::acl::acl_deluser(self, usernames).await?.convert()
    }
  }

  /// The command shows the available ACL categories if called without arguments. If a category name is given,
  /// the command shows all the Redis commands in the specified category.
  ///
  /// <https://redis.io/commands/acl-cat>
  fn acl_cat<R>(&self, category: Option<Str>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::acl::acl_cat(self, category).await?.convert() }
  }

  /// Generate a password with length `bits`, returning the password.
  ///
  /// <https://redis.io/commands/acl-genpass>
  fn acl_genpass<R>(&self, bits: Option<u16>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::acl::acl_genpass(self, bits).await?.convert() }
  }

  /// Return the username the current connection is authenticated with. New connections are authenticated
  /// with the "default" user.
  ///
  /// <https://redis.io/commands/acl-whoami>
  fn acl_whoami<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::acl::acl_whoami(self).await?.convert() }
  }

  /// Read `count` recent ACL security events.
  ///
  /// <https://redis.io/commands/acl-log>
  fn acl_log_count<R>(&self, count: Option<u32>) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::acl::acl_log_count(self, count).await?.convert() }
  }

  /// Clear the ACL security events logs.
  ///
  /// <https://redis.io/commands/acl-log>
  fn acl_log_reset(&self) -> impl Future<Output = FredResult<()>> + Send {
    async move { commands::acl::acl_log_reset(self).await }
  }
}
