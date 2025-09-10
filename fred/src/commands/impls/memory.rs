use super::*;
use crate::{
  protocol::{command::CommandKind, utils as protocol_utils},
  types::*,
  utils,
};

value_cmd!(memory_doctor, MemoryDoctor);
value_cmd!(memory_malloc_stats, MemoryMallocStats);
ok_cmd!(memory_purge, MemoryPurge);

pub async fn memory_stats<C: ClientLike>(client: &C) -> Result<Value, Error> {
  let response = utils::request_response(client, || Ok((CommandKind::MemoryStats, vec![]))).await?;
  protocol_utils::frame_to_results(response)
}

pub async fn memory_usage<C: ClientLike>(client: &C, key: Key, samples: Option<u32>) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(3);
    args.push(key.into());

    if let Some(samples) = samples {
      args.push(static_val!(SAMPLES));
      args.push(samples.into());
    }

    Ok((CommandKind::MemoryUsage, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}
