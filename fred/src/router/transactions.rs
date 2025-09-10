use crate::{
  error::{Error, ErrorKind},
  modules::inner::ClientInner,
  protocol::{
    command::{Command, CommandKind, ResponseSender},
    connection,
    connection::Connection,
    responders,
    utils as protocol_utils,
  },
  router::{utils, Router},
  runtime::RefCount,
  types::{config::Server, ClusterHash, Resp3Frame},
};

/// Send DISCARD to the provided server.
async fn discard(inner: &RefCount<ClientInner>, conn: &mut Connection) -> Result<(), Error> {
  let command = Command::new(CommandKind::Discard, vec![]);
  let frame = connection::request_response(inner, conn, command, Some(inner.internal_command_timeout())).await?;
  let result = protocol_utils::frame_to_results(frame)?;

  if result.is_ok() {
    Ok(())
  } else {
    Err(Error::new(ErrorKind::Unknown, "Unexpected DISCARD response."))
  }
}

/// Send EXEC to the provided server.
async fn exec(
  inner: &RefCount<ClientInner>,
  conn: &mut Connection,
  expected: usize,
) -> Result<Vec<Resp3Frame>, Error> {
  let mut command = Command::new(CommandKind::Exec, vec![]);
  let (frame, _) = utils::prepare_command(inner, &conn.counters, &mut command)?;
  conn.write(frame, true, false).await?;
  conn.flush().await?;
  let mut responses = Vec::with_capacity(expected + 1);

  for _ in 0 .. (expected + 1) {
    let frame = match conn.read_skip_pubsub(inner).await? {
      Some(frame) => frame,
      None => return Err(Error::new(ErrorKind::Protocol, "Unexpected empty frame received.")),
    };

    responses.push(frame);
  }
  responders::sample_command_latencies(inner, &mut command);
  Ok(responses)
}

fn update_hash_slot(commands: &mut [Command], slot: u16) {
  for command in commands.iter_mut() {
    command.hasher = ClusterHash::Custom(slot);
  }
}

fn max_attempts_error(tx: ResponseSender, error: Option<Error>) {
  let _ = tx.send(Err(
    error.unwrap_or_else(|| Error::new(ErrorKind::Unknown, "Max attempts exceeded")),
  ));
}

fn max_redirections_error(tx: ResponseSender) {
  let _ = tx.send(Err(Error::new(ErrorKind::Unknown, "Max redirections exceeded")));
}

fn is_execabort(error: &Error) -> bool {
  error.details().starts_with("EXECABORT")
}

fn process_responses(responses: Vec<Resp3Frame>, abort_on_error: bool) -> Result<Resp3Frame, Error> {
  // check for errors in intermediate frames then return the last frame
  let num_responses = responses.len();
  for (idx, frame) in responses.into_iter().enumerate() {
    if let Some(error) = protocol_utils::frame_to_error(&frame) {
      let should_return_error = error.is_moved()
        || error.is_ask()
        || is_execabort(&error)
        // return intermediate errors if `abort_on_error`
        || (idx < num_responses - 1 && abort_on_error)
        // always return errors from the last frame
        || idx == num_responses - 1;

      if should_return_error {
        return Err(error);
      } else {
        continue;
      }
    } else if idx == num_responses - 1 {
      return Ok(frame);
    }
  }

  Err(Error::new(ErrorKind::Protocol, "Missing transaction response."))
}

/// Send the transaction to the server.
pub async fn send(
  inner: &RefCount<ClientInner>,
  router: &mut Router,
  mut commands: Vec<Command>,
  id: u64,
  abort_on_error: bool,
  tx: ResponseSender,
) -> Result<(), Error> {
  if commands.is_empty() {
    let _ = tx.send(Err(Error::new(ErrorKind::InvalidCommand, "Empty transaction.")));
    return Ok(());
  }

  _debug!(inner, "Starting transaction {}", id);
  // command buffer length checked above
  let max_attempts = commands.last().unwrap().attempts_remaining;
  let max_redirections = commands.last().unwrap().redirections_remaining;
  let mut attempted = 0;
  let mut redirections = 0;
  let mut asking: Option<(Server, u16)> = None;

  'outer: loop {
    macro_rules! retry {
      ($err:expr) => {{
        attempted += 1;
        if attempted > max_attempts {
          max_attempts_error(tx, $err);
          return Ok(());
        } else {
          utils::reconnect_with_policy(inner, router).await?;
          continue 'outer;
        }
      }};
    }
    macro_rules! discard_retry {
      ($conn:expr, $err:expr) => {{
        let _ = $conn.skip_results(inner).await;
        let _ = discard(inner, $conn).await;
        retry!($err);
      }};
    }

    if let Err(err) = router.drain_all(inner).await {
      _debug!(inner, "Error draining router before transaction: {:?}", err);
      retry!(None);
    }
    // find the server that should receive the transaction
    let conn = match asking.as_ref() {
      Some((server, _)) => match router.get_connection_mut(server) {
        Some(conn) => conn,
        None => retry!(None),
      },
      None => match router.route(commands.last().unwrap()) {
        Some(server) => server,
        None => retry!(None),
      },
    };

    let expected = if asking.is_some() {
      commands.len() + 1
    } else {
      commands.len()
    };
    // sending ASKING first if needed
    if let Some((_, slot)) = asking.as_ref() {
      let mut command = Command::new_asking(*slot);
      let (frame, _) = match utils::prepare_command(inner, &conn.counters, &mut command) {
        Ok(frame) => frame,
        Err(err) => {
          let _ = tx.send(Err(err));
          return Ok(());
        },
      };

      if let Err(err) = conn.write(frame, true, false).await {
        _debug!(inner, "Error sending trx command: {:?}", err);
        retry!(Some(err));
      }
    }

    // write all the commands before EXEC
    for command in commands.iter_mut() {
      let (frame, _) = match utils::prepare_command(inner, &conn.counters, command) {
        Ok(frame) => frame,
        Err(err) => {
          let _ = tx.send(Err(err));
          return Ok(());
        },
      };
      if let Err(err) = conn.write(frame, true, false).await {
        _debug!(inner, "Error sending trx command: {:?}", err);
        discard_retry!(conn, Some(err));
      }
    }

    // send EXEC and process all the responses
    match exec(inner, conn, expected).await {
      Ok(responses) => match process_responses(responses, abort_on_error) {
        Ok(result) => {
          let _ = tx.send(Ok(result));
          return Ok(());
        },
        Err(err) => {
          if err.is_moved() {
            let slot = match protocol_utils::parse_cluster_error(err.details()) {
              Ok((_, slot, _)) => slot,
              Err(_) => {
                let _ = discard(inner, conn).await;
                let _ = tx.send(Err(Error::new(ErrorKind::Protocol, "Invalid cluster redirection.")));
                return Ok(());
              },
            };
            update_hash_slot(&mut commands, slot);

            redirections += 1;
            if redirections > max_redirections {
              max_redirections_error(tx);
              return Ok(());
            } else {
              Box::pin(utils::reconnect_with_policy(inner, router)).await?;
              continue;
            }
          } else if err.is_ask() {
            let (slot, server) = match protocol_utils::parse_cluster_error(err.details()) {
              Ok((_, slot, server)) => match Server::from_str(&server) {
                Some(server) => (slot, server),
                None => {
                  let _ = discard(inner, conn).await;
                  let _ = tx.send(Err(Error::new(ErrorKind::Protocol, "Invalid ASK cluster redirection.")));
                  return Ok(());
                },
              },
              Err(_) => {
                let _ = discard(inner, conn).await;
                let _ = tx.send(Err(Error::new(ErrorKind::Protocol, "Invalid cluster redirection.")));
                return Ok(());
              },
            };

            update_hash_slot(&mut commands, slot);
            redirections += 1;
            if redirections > max_redirections {
              max_redirections_error(tx);
              return Ok(());
            } else {
              asking = Some((server, slot));
              continue;
            }
          } else {
            let _ = discard(inner, conn).await;
            let _ = tx.send(Err(err));
            return Ok(());
          }
        },
      },
      Err(err) => {
        _debug!(inner, "Error writing transaction: {:?}", err);
        discard_retry!(conn, Some(err))
      },
    }
  }
}
