use crate::{
  error::Error,
  interfaces::{self, *},
  modules::{inner::ClientInner, response::FromValue},
  prelude::{FredResult, Value},
  protocol::{
    command::{Command, RouterCommand},
    responders::ResponseKind,
    utils as protocol_utils,
  },
  runtime::{oneshot_channel, Mutex, OneshotReceiver, RefCount},
  utils,
};
use std::{collections::VecDeque, fmt, fmt::Formatter};

fn clone_buffered_commands(buffer: &Mutex<VecDeque<Command>>) -> VecDeque<Command> {
  buffer.lock().iter().map(|c| c.duplicate(ResponseKind::Skip)).collect()
}

fn prepare_all_commands(
  commands: VecDeque<Command>,
  error_early: bool,
) -> (RouterCommand, OneshotReceiver<Result<Resp3Frame, Error>>) {
  let (tx, rx) = oneshot_channel();
  let expected_responses = commands
    .iter()
    .fold(0, |count, cmd| count + cmd.response.expected_response_frames());

  let mut response = ResponseKind::new_buffer_with_size(expected_responses, tx);
  response.set_error_early(error_early);

  let commands: Vec<Command> = commands
    .into_iter()
    .enumerate()
    .map(|(idx, mut cmd)| {
      cmd.response = response.duplicate().unwrap_or(ResponseKind::Skip);
      cmd.response.set_expected_index(idx);
      cmd
    })
    .collect();
  let command = RouterCommand::Pipeline { commands };

  (command, rx)
}

/// Send a series of commands in a [pipeline](https://redis.io/docs/manual/pipelining/).
///
/// See the [all](Self::all), [last](Self::last), and [try_all](Self::try_all) functions for more information.
pub struct Pipeline<C: ClientLike> {
  commands: RefCount<Mutex<VecDeque<Command>>>,
  client:   C,
}

#[doc(hidden)]
impl<C: ClientLike> Clone for Pipeline<C> {
  fn clone(&self) -> Self {
    Pipeline {
      commands: self.commands.clone(),
      client:   self.client.clone(),
    }
  }
}

impl<C: ClientLike> fmt::Debug for Pipeline<C> {
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    f.debug_struct("Pipeline")
      .field("client", &self.client.inner().id)
      .field("length", &self.commands.lock().len())
      .finish()
  }
}

#[doc(hidden)]
impl<C: ClientLike> From<C> for Pipeline<C> {
  fn from(client: C) -> Self {
    Pipeline {
      client,
      commands: RefCount::new(Mutex::new(VecDeque::new())),
    }
  }
}

impl<C: ClientLike> ClientLike for Pipeline<C> {
  #[doc(hidden)]
  fn inner(&self) -> &RefCount<ClientInner> {
    self.client.inner()
  }

  #[doc(hidden)]
  fn change_command(&self, command: &mut Command) {
    self.client.change_command(command);
  }

  #[doc(hidden)]
  #[allow(unused_mut)]
  fn send_command<T>(&self, command: T) -> Result<(), Error>
  where
    T: Into<Command>,
  {
    let mut command: Command = command.into();
    self.change_command(&mut command);

    if let Some(mut tx) = command.take_responder() {
      trace!(
        "{}: Respond early to {} command in pipeline.",
        &self.client.inner().id,
        command.kind.to_str_debug()
      );
      let _ = tx.send(Ok(protocol_utils::queued_frame()));
    }

    self.commands.lock().push_back(command);
    Ok(())
  }
}

#[cfg(feature = "i-acl")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-acl")))]
impl<C: AclInterface> AclInterface for Pipeline<C> {}
#[cfg(feature = "i-client")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-client")))]
impl<C: ClientInterface> ClientInterface for Pipeline<C> {}
#[cfg(feature = "i-cluster")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-cluster")))]
impl<C: ClusterInterface> ClusterInterface for Pipeline<C> {}
#[cfg(feature = "i-pubsub")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-pubsub")))]
impl<C: PubsubInterface> PubsubInterface for Pipeline<C> {}
#[cfg(feature = "i-config")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-config")))]
impl<C: ConfigInterface> ConfigInterface for Pipeline<C> {}
#[cfg(feature = "i-geo")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-geo")))]
impl<C: GeoInterface> GeoInterface for Pipeline<C> {}
#[cfg(feature = "i-hashes")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hashes")))]
impl<C: HashesInterface> HashesInterface for Pipeline<C> {}
#[cfg(feature = "i-hyperloglog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-hyperloglog")))]
impl<C: HyperloglogInterface> HyperloglogInterface for Pipeline<C> {}
#[cfg(feature = "i-keys")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-keys")))]
impl<C: KeysInterface> KeysInterface for Pipeline<C> {}
#[cfg(feature = "i-lists")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-lists")))]
impl<C: ListInterface> ListInterface for Pipeline<C> {}
#[cfg(feature = "i-memory")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-memory")))]
impl<C: MemoryInterface> MemoryInterface for Pipeline<C> {}
#[cfg(feature = "i-server")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-server")))]
impl<C: AuthInterface> AuthInterface for Pipeline<C> {}
#[cfg(feature = "i-server")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-server")))]
impl<C: ServerInterface> ServerInterface for Pipeline<C> {}
#[cfg(feature = "i-slowlog")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-slowlog")))]
impl<C: SlowlogInterface> SlowlogInterface for Pipeline<C> {}
#[cfg(feature = "i-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sets")))]
impl<C: SetsInterface> SetsInterface for Pipeline<C> {}
#[cfg(feature = "i-sorted-sets")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-sorted-sets")))]
impl<C: SortedSetsInterface> SortedSetsInterface for Pipeline<C> {}
#[cfg(feature = "i-streams")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-streams")))]
impl<C: StreamsInterface> StreamsInterface for Pipeline<C> {}
#[cfg(feature = "i-scripts")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-scripts")))]
impl<C: FunctionInterface> FunctionInterface for Pipeline<C> {}
#[cfg(feature = "i-redis-json")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redis-json")))]
impl<C: RedisJsonInterface> RedisJsonInterface for Pipeline<C> {}
#[cfg(feature = "i-time-series")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-time-series")))]
impl<C: TimeSeriesInterface> TimeSeriesInterface for Pipeline<C> {}
#[cfg(feature = "i-redisearch")]
#[cfg_attr(docsrs, doc(cfg(feature = "i-redisearch")))]
impl<C: RediSearchInterface> RediSearchInterface for Pipeline<C> {}

impl<C: ClientLike> Pipeline<C> {
  /// Send the pipeline and respond with an array of all responses.
  ///
  /// ```rust no_run
  /// # use fred::prelude::*;
  /// async fn example(client: &Client) -> Result<(), Error> {
  ///   let _ = client.mset(vec![("foo", 1), ("bar", 2)]).await?;
  ///
  ///   let pipeline = client.pipeline();
  ///   let _: () = pipeline.get("foo").await?; // returns when the command is queued in memory
  ///   let _: () = pipeline.get("bar").await?; // returns when the command is queued in memory
  ///
  ///   let results: Vec<i64> = pipeline.all().await?;
  ///   assert_eq!(results, vec![1, 2]);
  ///   Ok(())
  /// }
  /// ```
  pub async fn all<R>(&self) -> Result<R, Error>
  where
    R: FromValue,
  {
    let commands = clone_buffered_commands(&self.commands);
    send_all(self.client.inner(), commands).await?.convert()
  }

  /// Send the pipeline and respond with each individual result.
  ///
  /// Note: use `Value` as the return type (and [convert](crate::types::Value::convert) as needed) to
  /// support an array of different return types.
  ///
  /// ```rust no_run
  /// # use fred::prelude::*;
  /// async fn example(client: &Client) -> Result<(), Error> {
  ///   let _ = client.mset(vec![("foo", 1), ("bar", 2)]).await?;  
  ///
  ///   let pipeline = client.pipeline();
  ///   let _: () = pipeline.get("foo").await?;
  ///   let _: () = pipeline.hgetall("bar").await?; // this will error since `bar` is an integer
  ///
  ///   let results = pipeline.try_all::<Value>().await;
  ///   assert_eq!(results[0].clone()?.convert::<i64>()?, 1);
  ///   assert!(results[1].is_err());
  ///
  ///   Ok(())
  /// }
  /// ```
  pub async fn try_all<R>(&self) -> Vec<FredResult<R>>
  where
    R: FromValue,
  {
    let commands = clone_buffered_commands(&self.commands);
    try_send_all(self.client.inner(), commands)
      .await
      .into_iter()
      .map(|v| v.and_then(|v| v.convert()))
      .collect()
  }

  /// Send the pipeline and respond with only the result of the last command.
  ///
  /// ```rust no_run
  /// # use fred::prelude::*;
  /// async fn example(client: &Client) -> Result<(), Error> {
  ///   let pipeline = client.pipeline();
  ///   let _: () = pipeline.incr("foo").await?; // returns when the command is queued in memory
  ///   let _: () = pipeline.incr("foo").await?; // returns when the command is queued in memory
  ///
  ///   assert_eq!(pipeline.last::<i64>().await?, 2);
  ///   // pipelines can also be reused
  ///   assert_eq!(pipeline.last::<i64>().await?, 4);
  ///   Ok(())
  /// }
  /// ```
  pub async fn last<R>(&self) -> Result<R, Error>
  where
    R: FromValue,
  {
    let commands = clone_buffered_commands(&self.commands);
    send_last(self.client.inner(), commands).await?.convert()
  }
}

async fn try_send_all(inner: &RefCount<ClientInner>, commands: VecDeque<Command>) -> Vec<Result<Value, Error>> {
  if commands.is_empty() {
    return Vec::new();
  }

  let (mut command, rx) = prepare_all_commands(commands, false);
  command.inherit_options(inner);
  let timeout_dur = command.timeout_dur().unwrap_or_else(|| inner.default_command_timeout());

  if let Err(e) = interfaces::send_to_router(inner, command) {
    return vec![Err(e)];
  };
  let frame = match utils::timeout(rx, timeout_dur).await {
    Ok(result) => match result {
      Ok(f) => f,
      Err(e) => return vec![Err(e)],
    },
    Err(e) => return vec![Err(e)],
  };

  if let Resp3Frame::Array { data, .. } = frame {
    data.into_iter().map(protocol_utils::frame_to_results).collect()
  } else {
    vec![protocol_utils::frame_to_results(frame)]
  }
}

async fn send_all(inner: &RefCount<ClientInner>, commands: VecDeque<Command>) -> Result<Value, Error> {
  if commands.is_empty() {
    return Ok(Value::Array(Vec::new()));
  }

  let (mut command, rx) = prepare_all_commands(commands, true);
  command.inherit_options(inner);
  let timeout_dur = command.timeout_dur().unwrap_or_else(|| inner.default_command_timeout());

  interfaces::send_to_router(inner, command)?;
  let frame = utils::timeout(rx, timeout_dur).await??;
  protocol_utils::frame_to_results(frame)
}

async fn send_last(inner: &RefCount<ClientInner>, commands: VecDeque<Command>) -> Result<Value, Error> {
  if commands.is_empty() {
    return Ok(Value::Null);
  }

  let len = commands.len();
  let (tx, rx) = oneshot_channel();
  let mut commands: Vec<Command> = commands.into_iter().collect();
  commands[len - 1].response = ResponseKind::Respond(Some(tx));
  let mut command = RouterCommand::Pipeline { commands };
  command.inherit_options(inner);
  let timeout_dur = command.timeout_dur().unwrap_or_else(|| inner.default_command_timeout());

  interfaces::send_to_router(inner, command)?;
  let frame = utils::timeout(rx, timeout_dur).await??;
  protocol_utils::frame_to_results(frame)
}
