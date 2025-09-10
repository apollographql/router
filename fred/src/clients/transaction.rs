use crate::{
  error::{Error, ErrorKind},
  interfaces,
  interfaces::*,
  modules::inner::ClientInner,
  prelude::Value,
  protocol::{
    command::{Command, CommandKind, RouterCommand},
    hashers::ClusterHash,
    responders::ResponseKind,
    utils as protocol_utils,
  },
  runtime::{oneshot_channel, Mutex, RefCount},
  types::{
    config::{Options, Server},
    FromValue,
    Key,
  },
  utils,
};
use std::{collections::VecDeque, fmt};

struct State {
  id:        u64,
  commands:  Mutex<VecDeque<Command>>,
  watched:   Mutex<VecDeque<Key>>,
  hash_slot: Mutex<Option<u16>>,
}

/// A cheaply cloneable transaction block.
#[derive(Clone)]
#[cfg_attr(docsrs, doc(cfg(feature = "transactions")))]
pub struct Transaction {
  inner: RefCount<ClientInner>,
  state: RefCount<State>,
}

impl fmt::Debug for Transaction {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("Transaction")
      .field("client", &self.inner.id)
      .field("id", &self.state.id)
      .field("length", &self.state.commands.lock().len())
      .field("hash_slot", &self.state.hash_slot.lock())
      .finish()
  }
}

impl PartialEq for Transaction {
  fn eq(&self, other: &Self) -> bool {
    self.state.id == other.state.id
  }
}

impl Eq for Transaction {}

impl ClientLike for Transaction {
  #[doc(hidden)]
  fn inner(&self) -> &RefCount<ClientInner> {
    &self.inner
  }

  #[doc(hidden)]
  fn send_command<C>(&self, command: C) -> Result<(), Error>
  where
    C: Into<Command>,
  {
    let mut command: Command = command.into();

    self.disallow_all_cluster_commands(&command)?;
    // check cluster slot mappings as commands are added
    self.update_hash_slot(&command)?;

    #[allow(unused_mut)]
    if let Some(mut tx) = command.take_responder() {
      trace!(
        "{}: Respond early to {} command in transaction.",
        &self.inner.id,
        command.kind.to_str_debug()
      );
      let _ = tx.send(Ok(protocol_utils::queued_frame()));
    }

    self.state.commands.lock().push_back(command);
    Ok(())
  }
}

#[cfg(feature = "i-acl")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-acl")))]
impl AclInterface for Transaction {}
#[cfg(feature = "i-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-client")))]
impl ClientInterface for Transaction {}
#[cfg(feature = "i-pubsub")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-pubsub")))]
impl PubsubInterface for Transaction {}
#[cfg(feature = "i-config")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-config")))]
impl ConfigInterface for Transaction {}
#[cfg(feature = "i-geo")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-geo")))]
impl GeoInterface for Transaction {}
#[cfg(feature = "i-hashes")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hashes")))]
impl HashesInterface for Transaction {}
#[cfg(feature = "i-hyperloglog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hyperloglog")))]
impl HyperloglogInterface for Transaction {}
#[cfg(feature = "i-keys")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-keys")))]
impl KeysInterface for Transaction {}
#[cfg(feature = "i-lists")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-lists")))]
impl ListInterface for Transaction {}
#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
impl MemoryInterface for Transaction {}
impl AuthInterface for Transaction {}
#[cfg(feature = "i-server")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-server")))]
impl ServerInterface for Transaction {}
#[cfg(feature = "i-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sets")))]
impl SetsInterface for Transaction {}
#[cfg(feature = "i-sorted-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sorted-sets")))]
impl SortedSetsInterface for Transaction {}
#[cfg(feature = "i-streams")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-streams")))]
impl StreamsInterface for Transaction {}
#[cfg(feature = "i-scripts")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-scripts")))]
impl FunctionInterface for Transaction {}
#[cfg(feature = "i-redis-json")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redis-json")))]
impl RedisJsonInterface for Transaction {}
#[cfg(feature = "i-time-series")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
impl TimeSeriesInterface for Transaction {}
#[cfg(feature = "i-redisearch")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redisearch")))]
impl RediSearchInterface for Transaction {}

impl Transaction {
  /// Create a new transaction.
  pub(crate) fn from_inner(inner: &RefCount<ClientInner>) -> Self {
    Transaction {
      inner: inner.clone(),
      state: RefCount::new(State {
        commands:  Mutex::new(VecDeque::new()),
        watched:   Mutex::new(VecDeque::new()),
        hash_slot: Mutex::new(None),
        id:        utils::random_u64(u64::MAX),
      }),
    }
  }

  /// Check and update the hash slot for the transaction.
  pub(crate) fn update_hash_slot(&self, command: &Command) -> Result<(), Error> {
    if !self.inner.config.server.is_clustered() {
      return Ok(());
    }

    if let Some(slot) = command.cluster_hash() {
      if let Some(old_slot) = utils::read_mutex(&self.state.hash_slot) {
        let (old_server, server) = self.inner.with_cluster_state(|state| {
          debug!(
            "{}: Checking transaction hash slots: {}, {}",
            &self.inner.id, old_slot, slot
          );

          Ok((state.get_server(old_slot).cloned(), state.get_server(slot).cloned()))
        })?;

        if old_server != server {
          return Err(Error::new(
            ErrorKind::Cluster,
            "All transaction commands must use the same cluster node.",
          ));
        }
      } else {
        utils::set_mutex(&self.state.hash_slot, Some(slot));
      }
    }

    Ok(())
  }

  pub(crate) fn disallow_all_cluster_commands(&self, command: &Command) -> Result<(), Error> {
    if command.is_all_cluster_nodes() {
      Err(Error::new(
        ErrorKind::Cluster,
        "Cannot use concurrent cluster commands inside a transaction.",
      ))
    } else {
      Ok(())
    }
  }

  /// An ID identifying the underlying transaction state.
  pub fn id(&self) -> u64 {
    self.state.id
  }

  /// Clear the internal command buffer and watched keys.
  pub fn reset(&self) {
    self.state.commands.lock().clear();
    self.state.watched.lock().clear();
    self.state.hash_slot.lock().take();
  }

  /// Read the number of commands queued to run.
  pub fn len(&self) -> usize {
    self.state.commands.lock().len()
  }

  /// Executes all previously queued commands in a transaction.
  ///
  /// If `abort_on_error` is `true` the client will automatically send `DISCARD` if an error is received from
  /// any of the commands prior to `EXEC`. This does **not** apply to `MOVED` or `ASK` errors, which wll be followed
  /// automatically.
  ///
  /// <https://redis.io/commands/exec>
  ///
  /// ```rust no_run
  /// # use fred::prelude::*;
  ///
  /// async fn example(client: &Client) -> Result<(), Error> {
  ///   let _ = client.mset(vec![("foo", 1), ("bar", 2)]).await?;
  ///
  ///   let trx = client.multi();
  ///   let _: () = trx.get("foo").await?; // returns QUEUED
  ///   let _: () = trx.get("bar").await?; // returns QUEUED
  ///
  ///   let (foo, bar): (i64, i64) = trx.exec(false).await?;
  ///   assert_eq!((foo, bar), (1, 2));
  ///   Ok(())
  /// }
  /// ```
  pub async fn exec<R>(&self, abort_on_error: bool) -> Result<R, Error>
  where
    R: FromValue,
  {
    let commands = {
      self
        .state
        .commands
        .lock()
        .iter()
        .map(|cmd| cmd.duplicate(ResponseKind::Skip))
        .collect()
    };
    let hash_slot = utils::read_mutex(&self.state.hash_slot);

    exec(&self.inner, commands, hash_slot, abort_on_error, self.state.id)
      .await?
      .convert()
  }

  /// Read the hash slot against which this transaction will run, if known.
  pub fn hash_slot(&self) -> Option<u16> {
    utils::read_mutex(&self.state.hash_slot)
  }

  /// Read the server ID against which this transaction will run, if known.
  pub fn cluster_node(&self) -> Option<Server> {
    utils::read_mutex(&self.state.hash_slot).and_then(|slot| {
      self
        .inner
        .with_cluster_state(|state| Ok(state.get_server(slot).cloned()))
        .ok()
        .and_then(|server| server)
    })
  }
}

async fn exec(
  inner: &RefCount<ClientInner>,
  commands: VecDeque<Command>,
  hash_slot: Option<u16>,
  abort_on_error: bool,
  id: u64,
) -> Result<Value, Error> {
  if commands.is_empty() {
    return Ok(Value::Null);
  }
  let (tx, rx) = oneshot_channel();
  let trx_options = Options::from_command(&commands[0]);

  let mut multi = Command::new(CommandKind::Multi, vec![]);
  trx_options.apply(&mut multi);

  let commands: Vec<Command> = [multi]
    .into_iter()
    .chain(commands.into_iter())
    .map(|mut command| {
      command.inherit_options(inner);
      command.response = ResponseKind::Skip;
      command.can_pipeline = true;
      command.transaction_id = Some(id);
      command.use_replica = false;
      if let Some(hash_slot) = hash_slot.as_ref() {
        command.hasher = ClusterHash::Custom(*hash_slot);
      }
      command
    })
    .collect();

  _trace!(
    inner,
    "Sending transaction {} with {} commands to router.",
    id,
    commands.len(),
  );
  let command = RouterCommand::Transaction {
    id,
    tx,
    commands,
    abort_on_error,
  };
  let timeout_dur = trx_options.timeout.unwrap_or_else(|| inner.default_command_timeout());

  interfaces::send_to_router(inner, command)?;
  let frame = utils::timeout(rx, timeout_dur).await??;
  protocol_utils::frame_to_results(frame)
}
