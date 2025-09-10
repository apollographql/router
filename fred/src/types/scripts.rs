#[cfg(feature = "sha-1")]
use crate::util::sha1_hash;
use crate::{
  clients::Client,
  interfaces::{FunctionInterface, LuaInterface},
  prelude::{Error, ErrorKind, FredResult, FromValue},
  types::{MultipleKeys, MultipleValues, Value},
  utils,
};
use bytes_utils::Str;
use std::{
  cmp::Ordering,
  collections::HashMap,
  convert::TryInto,
  fmt,
  fmt::Formatter,
  hash::{Hash, Hasher},
  ops::Deref,
};

/// Flags for the SCRIPT DEBUG command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptDebugFlag {
  Yes,
  No,
  Sync,
}

impl ScriptDebugFlag {
  #[cfg(feature = "i-scripts")]
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      ScriptDebugFlag::Yes => "YES",
      ScriptDebugFlag::No => "NO",
      ScriptDebugFlag::Sync => "SYNC",
    })
  }
}

/// The policy type for the [FUNCTION RESTORE](https://redis.io/commands/function-restore/) command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FnPolicy {
  Flush,
  Append,
  Replace,
}

impl Default for FnPolicy {
  fn default() -> Self {
    FnPolicy::Append
  }
}

impl FnPolicy {
  #[cfg(feature = "i-scripts")]
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      FnPolicy::Flush => "FLUSH",
      FnPolicy::Append => "APPEND",
      FnPolicy::Replace => "REPLACE",
    })
  }

  pub(crate) fn from_str(s: &str) -> Result<Self, Error> {
    Ok(match s {
      "flush" | "FLUSH" => FnPolicy::Flush,
      "append" | "APPEND" => FnPolicy::Append,
      "replace" | "REPLACE" => FnPolicy::Replace,
      _ => {
        return Err(Error::new(
          ErrorKind::InvalidArgument,
          "Invalid function restore policy.",
        ))
      },
    })
  }
}

// have to implement these for specific types to avoid conflicting with the core Into implementation
impl TryFrom<&str> for FnPolicy {
  type Error = Error;

  fn try_from(value: &str) -> Result<Self, Self::Error> {
    FnPolicy::from_str(value)
  }
}

impl TryFrom<&String> for FnPolicy {
  type Error = Error;

  fn try_from(value: &String) -> Result<Self, Self::Error> {
    FnPolicy::from_str(value.as_str())
  }
}

impl TryFrom<String> for FnPolicy {
  type Error = Error;

  fn try_from(value: String) -> Result<Self, Self::Error> {
    FnPolicy::from_str(value.as_str())
  }
}

impl TryFrom<Str> for FnPolicy {
  type Error = Error;

  fn try_from(value: Str) -> Result<Self, Self::Error> {
    FnPolicy::from_str(&value)
  }
}

impl TryFrom<&Str> for FnPolicy {
  type Error = Error;

  fn try_from(value: &Str) -> Result<Self, Self::Error> {
    FnPolicy::from_str(value)
  }
}

/// An interface for caching and running lua scripts.
///
/// ```rust no_run
/// # use fred::types::scripts::Script;
/// # use fred::prelude::*;
/// async fn example(client: &Client) -> Result<(), Error> {
///   let script = Script::from_lua("return ARGV[1]");
///   assert_eq!(script.sha1(), "098e0f0d1448c0a81dafe820f66d460eb09263da");
///
///   let _ = script.load(client).await?;
///   let result: String = script.evalsha(client, "key", "arg").await?;
///   assert_eq!(result, "arg");
///   Ok(())
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Script {
  lua:  Option<Str>,
  hash: Str,
}

impl fmt::Display for Script {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.hash)
  }
}

impl Hash for Script {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.hash.hash(state);
  }
}

impl Ord for Script {
  fn cmp(&self, other: &Self) -> Ordering {
    self.hash.cmp(&other.hash)
  }
}

impl PartialOrd for Script {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Script {
  /// Create a new `Script` from a lua script.
  #[cfg(feature = "sha-1")]
  #[cfg_attr(docsrs, doc(cfg(feature = "sha-1")))]
  pub fn from_lua<S: Into<Str>>(lua: S) -> Self {
    let lua: Str = lua.into();
    let hash = Str::from(sha1_hash(&lua));

    Script { lua: Some(lua), hash }
  }

  /// Create a new `Script` from a lua hash.
  pub fn from_hash<S: Into<Str>>(hash: S) -> Self {
    Script {
      lua:  None,
      hash: hash.into(),
    }
  }

  /// Read the lua script contents.
  pub fn lua(&self) -> Option<&Str> {
    self.lua.as_ref()
  }

  /// Read the SHA-1 hash for the script.
  pub fn sha1(&self) -> &Str {
    &self.hash
  }

  /// Call `SCRIPT LOAD` on all the associated servers. This must be
  /// called once before calling [evalsha](Self::evalsha).
  #[cfg(feature = "sha-1")]
  #[cfg_attr(docsrs, doc(cfg(feature = "sha-1")))]
  pub async fn load(&self, client: &Client) -> FredResult<()> {
    if let Some(ref lua) = self.lua {
      client.script_load_cluster::<(), _>(lua.clone()).await
    } else {
      Err(Error::new(ErrorKind::Unknown, "Missing lua script contents."))
    }
  }

  /// Send `EVALSHA` to the server with the provided arguments.
  pub async fn evalsha<R, C, K, V>(&self, client: &C, keys: K, args: V) -> FredResult<R>
  where
    R: FromValue,
    C: LuaInterface + Send + Sync,
    K: Into<MultipleKeys> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    client.evalsha(self.hash.clone(), keys, args).await
  }

  /// Send `EVALSHA` to the server with the provided arguments. Automatically `SCRIPT LOAD` in case
  /// of `NOSCRIPT` error and try `EVALSHA` again.
  #[cfg(feature = "sha-1")]
  #[cfg_attr(docsrs, doc(cfg(feature = "sha-1")))]
  pub async fn evalsha_with_reload<R, K, V>(&self, client: &Client, keys: K, args: V) -> FredResult<R>
  where
    R: FromValue,
    K: Into<MultipleKeys> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    into!(keys);
    try_into!(args);

    match client.evalsha(self.hash.clone(), keys.clone(), args.clone()).await {
      Err(error) if error.details().starts_with("NOSCRIPT") => {
        self.load(client).await?;
        client.evalsha(self.hash.clone(), keys, args).await
      },
      result => result,
    }
  }
}

/// Possible [flags](https://redis.io/docs/manual/programmability/lua-api/) associated with a [Function](crate::types::scripts::Function).
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum FunctionFlag {
  NoWrites,
  AllowOOM,
  NoCluster,
  AllowCrossSlotKeys,
  AllowStale,
}

impl FunctionFlag {
  /// Parse the string representation of the flag.
  #[allow(clippy::should_implement_trait)]
  pub fn from_str(s: &str) -> Option<Self> {
    Some(match s {
      "allow-oom" => FunctionFlag::AllowOOM,
      "allow-stale" => FunctionFlag::AllowStale,
      "allow-cross-slot-keys" => FunctionFlag::AllowCrossSlotKeys,
      "no-writes" => FunctionFlag::NoWrites,
      "no-cluster" => FunctionFlag::NoCluster,
      _ => return None,
    })
  }

  /// Convert to the string representation of the flag.
  pub fn to_str(&self) -> &'static str {
    match self {
      FunctionFlag::AllowCrossSlotKeys => "allow-cross-slot-keys",
      FunctionFlag::AllowOOM => "allow-oom",
      FunctionFlag::NoCluster => "no-cluster",
      FunctionFlag::NoWrites => "no-writes",
      FunctionFlag::AllowStale => "allow-stale",
    }
  }
}

/// An individual function within a [Library](crate::types::scripts::Library).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Function {
  pub(crate) name:  Str,
  pub(crate) flags: Vec<FunctionFlag>,
}

impl fmt::Display for Function {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.name)
  }
}

impl Hash for Function {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.name.hash(state);
  }
}

impl Ord for Function {
  fn cmp(&self, other: &Self) -> Ordering {
    self.name.cmp(&other.name)
  }
}

impl PartialOrd for Function {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Function {
  /// Create a new `Function`.
  pub fn new<S: Into<Str>>(name: S, flags: Vec<FunctionFlag>) -> Self {
    Function {
      name: name.into(),
      flags,
    }
  }

  /// Read the name of the function.
  pub fn name(&self) -> &Str {
    &self.name
  }

  /// Read the flags associated with the function.
  pub fn flags(&self) -> &[FunctionFlag] {
    &self.flags
  }

  /// Send the [fcall](crate::interfaces::FunctionInterface::fcall) command via the provided client.
  pub async fn fcall<R, C, K, V>(&self, client: &C, keys: K, args: V) -> FredResult<R>
  where
    R: FromValue,
    C: FunctionInterface + Send + Sync,
    K: Into<MultipleKeys> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    client.fcall(self.name.clone(), keys, args).await
  }

  /// Send the [fcall_ro](crate::interfaces::FunctionInterface::fcall_ro) command via the provided client.
  pub async fn fcall_ro<R, C, K, V>(&self, client: &C, keys: K, args: V) -> FredResult<R>
  where
    R: FromValue,
    C: FunctionInterface + Send + Sync,
    K: Into<MultipleKeys> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    client.fcall_ro(self.name.clone(), keys, args).await
  }
}

/// A helper struct for interacting with [libraries and functions](https://redis.io/docs/manual/programmability/functions-intro/).
///
/// ```rust no_run
/// # use fred::types::scripts::{FunctionFlag, Library};
/// let code = "#!lua name=mylib \n redis.register_function('myfunc', function(keys, args) return \
///             args[1] end)";
/// let library = Library::from_code(client, code).await?;
/// assert_eq!(library.name(), "mylib");
///
/// if let Some(func) = library.functions().get("myfunc") {
///   if func.flags().contains(&FunctionFlag::NoWrites) {
///     let _: () = func.fcall_ro(client, "key", "arg").await?;
///   } else {
///     let _: () = func.fcall(client, "key", "arg").await?;
///   }
/// }
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Library {
  name:      Str,
  functions: HashMap<Str, Function>,
}

impl fmt::Display for Library {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    write!(f, "{}", self.name)
  }
}

impl Hash for Library {
  fn hash<H: Hasher>(&self, state: &mut H) {
    self.name.hash(state);
  }
}

impl Ord for Library {
  fn cmp(&self, other: &Self) -> Ordering {
    self.name.cmp(&other.name)
  }
}

impl PartialOrd for Library {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Library {
  /// Create a new `Library` with the provided code, loading it on all the servers and inspecting the contents via the [FUNCTION LIST](https://redis.io/commands/function-list/) command.
  ///
  /// This interface will load the library on the server.
  pub async fn from_code<S>(client: &Client, code: S) -> Result<Self, Error>
  where
    S: Into<Str>,
  {
    let code = code.into();
    let name: Str = client.function_load_cluster(true, code).await?;
    let functions = client
      .function_list::<Value, _>(Some(name.deref()), false)
      .await?
      .as_functions(&name)?;

    Ok(Library {
      name,
      functions: functions.into_iter().map(|f| (f.name.clone(), f)).collect(),
    })
  }

  /// Create a new `Library` with the associated name, inspecting the library contents via the [FUNCTION LIST](https://redis.io/commands/function-list/) command.
  ///
  /// This interface assumes the library is already loaded on the server.
  pub async fn from_name<S>(client: &Client, name: S) -> Result<Self, Error>
  where
    S: Into<Str>,
  {
    let name = name.into();
    let functions = client
      .function_list::<Value, _>(Some(name.deref()), false)
      .await?
      .as_functions(&name)?;

    Ok(Library {
      name,
      functions: functions.into_iter().map(|f| (f.name.clone(), f)).collect(),
    })
  }

  /// Read the name of the library.
  pub fn name(&self) -> &Str {
    &self.name
  }

  /// Read the functions contained within this library.
  pub fn functions(&self) -> &HashMap<Str, Function> {
    &self.functions
  }
}
