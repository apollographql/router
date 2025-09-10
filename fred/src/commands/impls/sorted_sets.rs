use super::*;
use crate::{
  error::*,
  protocol::{command::CommandKind, utils as protocol_utils},
  types::{
    sorted_sets::{
      AggregateOptions,
      MultipleWeights,
      MultipleZaddValues,
      Ordering,
      ZCmp,
      ZRange,
      ZRangeBound,
      ZSort,
    },
    *,
  },
  utils,
};
use std::convert::TryInto;

static INCR: &str = "INCR";
static WITH_SCORES: &str = "WITHSCORES";
static WITH_SCORE: &str = "WITHSCORE";
static AGGREGATE: &str = "AGGREGATE";
static WEIGHTS: &str = "WEIGHTS";

fn new_range_error(kind: &Option<ZSort>) -> Result<(), Error> {
  if let Some(ref sort) = *kind {
    Err(Error::new(
      ErrorKind::InvalidArgument,
      format!("Invalid range bound with {} sort", sort.to_str()),
    ))
  } else {
    Err(Error::new(ErrorKind::InvalidArgument, "Invalid index range bound."))
  }
}

fn check_range_type(range: &ZRange, kind: &Option<ZSort>) -> Result<(), Error> {
  match kind {
    Some(_kind) => match _kind {
      ZSort::ByLex => match range.range {
        ZRangeBound::Lex(_) | ZRangeBound::InfiniteLex | ZRangeBound::NegInfinityLex => Ok(()),
        _ => new_range_error(kind),
      },
      ZSort::ByScore => match range.range {
        ZRangeBound::Score(_) | ZRangeBound::InfiniteScore | ZRangeBound::NegInfiniteScore => Ok(()),
        _ => new_range_error(kind),
      },
    },
    None => match range.range {
      ZRangeBound::Index(_) => Ok(()),
      _ => new_range_error(kind),
    },
  }
}

fn check_range_types(min: &ZRange, max: &ZRange, kind: &Option<ZSort>) -> Result<(), Error> {
  check_range_type(min, kind)?;
  check_range_type(max, kind)?;
  Ok(())
}

pub async fn bzmpop<C: ClientLike>(
  client: &C,
  timeout: f64,
  keys: MultipleKeys,
  sort: ZCmp,
  count: Option<i64>,
) -> Result<Value, Error> {
  let timeout: Value = timeout.try_into()?;

  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(keys.len() + 4);
    args.push(timeout);
    args.push(keys.len().try_into()?);
    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    args.push(sort.to_str().into());
    if let Some(count) = count {
      args.push(static_val!(COUNT));
      args.push(count.into());
    }

    Ok((CommandKind::BzmPop, args))
  })
  .await?;

  protocol_utils::check_null_timeout(&frame)?;
  protocol_utils::frame_to_results(frame)
}

pub async fn bzpopmin<C: ClientLike>(client: &C, keys: MultipleKeys, timeout: f64) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(1 + keys.len());

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    args.push(timeout.try_into()?);

    Ok((CommandKind::BzPopMin, args))
  })
  .await?;

  protocol_utils::check_null_timeout(&frame)?;
  protocol_utils::frame_to_results(frame)
}

pub async fn bzpopmax<C: ClientLike>(client: &C, keys: MultipleKeys, timeout: f64) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(1 + keys.len());

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    args.push(timeout.try_into()?);

    Ok((CommandKind::BzPopMax, args))
  })
  .await?;

  protocol_utils::check_null_timeout(&frame)?;
  protocol_utils::frame_to_results(frame)
}

pub async fn zadd<C: ClientLike>(
  client: &C,
  key: Key,
  options: Option<SetOptions>,
  ordering: Option<Ordering>,
  changed: bool,
  incr: bool,
  values: MultipleZaddValues,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(5 + (values.len() * 2));
    args.push(key.into());

    if let Some(options) = options {
      args.push(options.to_str().into());
    }
    if let Some(ordering) = ordering {
      args.push(ordering.to_str().into());
    }
    if changed {
      args.push(static_val!(CHANGED));
    }
    if incr {
      args.push(static_val!(INCR));
    }

    for (score, value) in values.inner().into_iter() {
      args.push(score.try_into()?);
      args.push(value);
    }

    Ok((CommandKind::Zadd, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zcard<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_value_cmd(client, CommandKind::Zcard, key.into()).await
}

pub async fn zcount<C: ClientLike>(client: &C, key: Key, min: f64, max: f64) -> Result<Value, Error> {
  let (min, max) = (min.try_into()?, max.try_into()?);
  args_value_cmd(client, CommandKind::Zcount, vec![key.into(), min, max]).await
}

pub async fn zdiff<C: ClientLike>(client: &C, keys: MultipleKeys, withscores: bool) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2 + keys.len());
    args.push(keys.len().try_into()?);

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    if withscores {
      args.push(static_val!(WITH_SCORES));
    }

    Ok((CommandKind::Zdiff, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zdiffstore<C: ClientLike>(client: &C, dest: Key, keys: MultipleKeys) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2 + keys.len());
    args.push(dest.into());
    args.push(keys.len().try_into()?);

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    Ok((CommandKind::Zdiffstore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zincrby<C: ClientLike>(client: &C, key: Key, increment: f64, member: Value) -> Result<Value, Error> {
  let increment = increment.try_into()?;
  let args = vec![key.into(), increment, member];
  args_value_cmd(client, CommandKind::Zincrby, args).await
}

pub async fn zinter<C: ClientLike>(
  client: &C,
  keys: MultipleKeys,
  weights: MultipleWeights,
  aggregate: Option<AggregateOptions>,
  withscores: bool,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args_len = 6 + keys.len() + weights.len();
    let mut args = Vec::with_capacity(args_len);
    args.push(keys.len().try_into()?);

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    if weights.len() > 0 {
      args.push(static_val!(WEIGHTS));
      for weight in weights.inner().into_iter() {
        args.push(weight.try_into()?);
      }
    }
    if let Some(options) = aggregate {
      args.push(static_val!(AGGREGATE));
      args.push(options.to_str().into());
    }
    if withscores {
      args.push(static_val!(WITH_SCORES));
    }

    Ok((CommandKind::Zinter, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zinterstore<C: ClientLike>(
  client: &C,
  dest: Key,
  keys: MultipleKeys,
  weights: MultipleWeights,
  aggregate: Option<AggregateOptions>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args_len = 5 + keys.len() + weights.len();
    let mut args = Vec::with_capacity(args_len);
    args.push(dest.into());
    args.push(keys.len().try_into()?);

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    if weights.len() > 0 {
      args.push(static_val!(WEIGHTS));
      for weight in weights.inner().into_iter() {
        args.push(weight.try_into()?);
      }
    }
    if let Some(options) = aggregate {
      args.push(static_val!(AGGREGATE));
      args.push(options.to_str().into());
    }

    Ok((CommandKind::Zinterstore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zlexcount<C: ClientLike>(client: &C, key: Key, min: ZRange, max: ZRange) -> Result<Value, Error> {
  check_range_types(&min, &max, &Some(ZSort::ByLex))?;

  let args = vec![key.into(), min.into_value()?, max.into_value()?];
  args_value_cmd(client, CommandKind::Zlexcount, args).await
}

pub async fn zpopmax<C: ClientLike>(client: &C, key: Key, count: Option<usize>) -> Result<Value, Error> {
  let args = if let Some(count) = count {
    vec![key.into(), count.try_into()?]
  } else {
    vec![key.into()]
  };

  args_values_cmd(client, CommandKind::Zpopmax, args).await
}

pub async fn zpopmin<C: ClientLike>(client: &C, key: Key, count: Option<usize>) -> Result<Value, Error> {
  let args = if let Some(count) = count {
    vec![key.into(), count.try_into()?]
  } else {
    vec![key.into()]
  };

  args_values_cmd(client, CommandKind::Zpopmin, args).await
}

pub async fn zmpop<C: ClientLike>(
  client: &C,
  keys: MultipleKeys,
  sort: ZCmp,
  count: Option<i64>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(keys.len() + 3);
    args.push(keys.len().try_into()?);
    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    args.push(sort.to_str().into());
    if let Some(count) = count {
      args.push(static_val!(COUNT));
      args.push(count.into());
    }

    Ok((CommandKind::Zmpop, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zrandmember<C: ClientLike>(client: &C, key: Key, count: Option<(i64, bool)>) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(3);
    args.push(key.into());

    if let Some((count, withscores)) = count {
      args.push(count.into());
      if withscores {
        args.push(static_val!(WITH_SCORES));
      }
    }

    Ok((CommandKind::Zrandmember, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zrangestore<C: ClientLike>(
  client: &C,
  dest: Key,
  source: Key,
  min: ZRange,
  max: ZRange,
  sort: Option<ZSort>,
  rev: bool,
  limit: Option<Limit>,
) -> Result<Value, Error> {
  check_range_types(&min, &max, &sort)?;

  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(9);
    args.push(dest.into());
    args.push(source.into());
    args.push(min.into_value()?);
    args.push(max.into_value()?);

    if let Some(sort) = sort {
      args.push(sort.to_str().into());
    }
    if rev {
      args.push(static_val!(REV));
    }
    if let Some((offset, count)) = limit {
      args.push(static_val!(LIMIT));
      args.push(offset.into());
      args.push(count.into());
    }

    Ok((CommandKind::Zrangestore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zrange<C: ClientLike>(
  client: &C,
  key: Key,
  min: ZRange,
  max: ZRange,
  sort: Option<ZSort>,
  rev: bool,
  limit: Option<Limit>,
  withscores: bool,
) -> Result<Value, Error> {
  check_range_types(&min, &max, &sort)?;

  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(9);
    args.push(key.into());
    args.push(min.into_value()?);
    args.push(max.into_value()?);

    if let Some(sort) = sort {
      args.push(sort.to_str().into());
    }
    if rev {
      args.push(static_val!(REV));
    }
    if let Some((offset, count)) = limit {
      args.push(static_val!(LIMIT));
      args.push(offset.into());
      args.push(count.into());
    }
    if withscores {
      args.push(static_val!(WITH_SCORES));
    }

    Ok((CommandKind::Zrange, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zrangebylex<C: ClientLike>(
  client: &C,
  key: Key,
  min: ZRange,
  max: ZRange,
  limit: Option<Limit>,
) -> Result<Value, Error> {
  check_range_types(&min, &max, &Some(ZSort::ByLex))?;

  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(6);
    args.push(key.into());
    args.push(min.into_value()?);
    args.push(max.into_value()?);

    if let Some((offset, count)) = limit {
      args.push(static_val!(LIMIT));
      args.push(offset.into());
      args.push(count.into());
    }

    Ok((CommandKind::Zrangebylex, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zrevrangebylex<C: ClientLike>(
  client: &C,
  key: Key,
  max: ZRange,
  min: ZRange,
  limit: Option<Limit>,
) -> Result<Value, Error> {
  check_range_types(&min, &max, &Some(ZSort::ByLex))?;

  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(6);
    args.push(key.into());
    args.push(max.into_value()?);
    args.push(min.into_value()?);

    if let Some((offset, count)) = limit {
      args.push(static_val!(LIMIT));
      args.push(offset.into());
      args.push(count.into());
    }

    Ok((CommandKind::Zrevrangebylex, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zrangebyscore<C: ClientLike>(
  client: &C,
  key: Key,
  min: ZRange,
  max: ZRange,
  withscores: bool,
  limit: Option<Limit>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(7);
    args.push(key.into());
    args.push(min.into_value()?);
    args.push(max.into_value()?);

    if withscores {
      args.push(static_val!(WITH_SCORES));
    }
    if let Some((offset, count)) = limit {
      args.push(static_val!(LIMIT));
      args.push(offset.into());
      args.push(count.into());
    }

    Ok((CommandKind::Zrangebyscore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zrevrangebyscore<C: ClientLike>(
  client: &C,
  key: Key,
  max: ZRange,
  min: ZRange,
  withscores: bool,
  limit: Option<Limit>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(7);
    args.push(key.into());
    args.push(max.into_value()?);
    args.push(min.into_value()?);

    if withscores {
      args.push(static_val!(WITH_SCORES));
    }
    if let Some((offset, count)) = limit {
      args.push(static_val!(LIMIT));
      args.push(offset.into());
      args.push(count.into());
    }

    Ok((CommandKind::Zrevrangebyscore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zrank<C: ClientLike>(client: &C, key: Key, member: Value, withscore: bool) -> Result<Value, Error> {
  let mut args = vec![key.into(), member];
  if withscore {
    args.push(static_val!(WITH_SCORE));
  }

  args_value_cmd(client, CommandKind::Zrank, args).await
}

pub async fn zrem<C: ClientLike>(client: &C, key: Key, members: MultipleValues) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let members = members.into_multiple_values();
    let mut args = Vec::with_capacity(1 + members.len());
    args.push(key.into());

    for member in members.into_iter() {
      args.push(member);
    }
    Ok((CommandKind::Zrem, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zremrangebylex<C: ClientLike>(client: &C, key: Key, min: ZRange, max: ZRange) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    check_range_types(&min, &max, &Some(ZSort::ByLex))?;

    Ok((CommandKind::Zremrangebylex, vec![
      key.into(),
      min.into_value()?,
      max.into_value()?,
    ]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zremrangebyrank<C: ClientLike>(client: &C, key: Key, start: i64, stop: i64) -> Result<Value, Error> {
  let (start, stop) = (start.into(), stop.into());
  args_value_cmd(client, CommandKind::Zremrangebyrank, vec![key.into(), start, stop]).await
}

pub async fn zremrangebyscore<C: ClientLike>(client: &C, key: Key, min: ZRange, max: ZRange) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    check_range_types(&min, &max, &Some(ZSort::ByScore))?;

    Ok((CommandKind::Zremrangebyscore, vec![
      key.into(),
      min.into_value()?,
      max.into_value()?,
    ]))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zrevrange<C: ClientLike>(
  client: &C,
  key: Key,
  start: i64,
  stop: i64,
  withscores: bool,
) -> Result<Value, Error> {
  let (start, stop) = (start.into(), stop.into());
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(4);
    args.push(key.into());
    args.push(start);
    args.push(stop);

    if withscores {
      args.push(static_val!(WITH_SCORES));
    }

    Ok((CommandKind::Zrevrange, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zrevrank<C: ClientLike>(client: &C, key: Key, member: Value, withscore: bool) -> Result<Value, Error> {
  let mut args = vec![key.into(), member];
  if withscore {
    args.push(static_val!(WITH_SCORE));
  }

  args_value_cmd(client, CommandKind::Zrevrank, args).await
}

pub async fn zscore<C: ClientLike>(client: &C, key: Key, member: Value) -> Result<Value, Error> {
  args_value_cmd(client, CommandKind::Zscore, vec![key.into(), member]).await
}

pub async fn zunion<C: ClientLike>(
  client: &C,
  keys: MultipleKeys,
  weights: MultipleWeights,
  aggregate: Option<AggregateOptions>,
  withscores: bool,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args_len = keys.len() + weights.len();
    let mut args = Vec::with_capacity(5 + args_len);
    args.push(keys.len().try_into()?);

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    if weights.len() > 0 {
      args.push(static_val!(WEIGHTS));
      for weight in weights.inner().into_iter() {
        args.push(weight.try_into()?);
      }
    }

    if let Some(aggregate) = aggregate {
      args.push(static_val!(AGGREGATE));
      args.push(aggregate.to_str().into());
    }
    if withscores {
      args.push(static_val!(WITH_SCORES));
    }

    Ok((CommandKind::Zunion, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zunionstore<C: ClientLike>(
  client: &C,
  dest: Key,
  keys: MultipleKeys,
  weights: MultipleWeights,
  aggregate: Option<AggregateOptions>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let args_len = keys.len() + weights.len();
    let mut args = Vec::with_capacity(5 + args_len);
    args.push(dest.into());
    args.push(keys.len().try_into()?);

    for key in keys.inner().into_iter() {
      args.push(key.into());
    }
    if weights.len() > 0 {
      args.push(static_val!(WEIGHTS));
      for weight in weights.inner().into_iter() {
        args.push(weight.try_into()?);
      }
    }

    if let Some(aggregate) = aggregate {
      args.push(static_val!(AGGREGATE));
      args.push(aggregate.to_str().into());
    }

    Ok((CommandKind::Zunionstore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn zmscore<C: ClientLike>(client: &C, key: Key, members: MultipleValues) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let members = members.into_multiple_values();
    let mut args = Vec::with_capacity(1 + members.len());
    args.push(key.into());

    for member in members.into_iter() {
      args.push(member);
    }
    Ok((CommandKind::Zmscore, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}
