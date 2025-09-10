use crate::{
  commands::{args_values_cmd, one_arg_values_cmd, COUNT, LEN, LIMIT},
  error::Error,
  interfaces::ClientLike,
  protocol::{command::CommandKind, utils as protocol_utils},
  types::{
    redisearch::{
      AggregateOperation,
      FtAggregateOptions,
      FtAlterOptions,
      FtCreateOptions,
      FtSearchOptions,
      Load,
      SearchSchema,
      SearchSchemaKind,
      SpellcheckTerms,
    },
    Key,
    MultipleStrings,
    Value,
  },
  utils,
};
use bytes::Bytes;
use bytes_utils::Str;

static DD: &str = "DD";
static DIALECT: &str = "DIALECT";
static DISTANCE: &str = "DISTANCE";
static INCLUDE: &str = "INCLUDE";
static EXCLUDE: &str = "EXCLUDE";
static TERMS: &str = "TERMS";
static INCR: &str = "INCR";
static PAYLOAD: &str = "PAYLOAD";
static FUZZY: &str = "FUZZY";
static WITHSCORES: &str = "WITHSCORES";
static WITHPAYLOADS: &str = "WITHPAYLOADS";
static MAX: &str = "MAX";
static SKIPINITIALSCAN: &str = "SKIPINITIALSCAN";
static NOCONTENT: &str = "NOCONTENT";
static VERBATIM: &str = "VERBATIM";
static NOSTOPWORDS: &str = "NOSTOPWORDS";
static WITHSORTKEYS: &str = "WITHSORTKEYS";
static FILTER: &str = "FILTER";
static GEOFILTER: &str = "GEOFILTER";
static INKEYS: &str = "INKEYS";
static INFIELDS: &str = "INFIELDS";
static _RETURN: &str = "RETURN";
static AS: &str = "AS";
static SUMMARIZE: &str = "SUMMARIZE";
static FIELDS: &str = "FIELDS";
static FRAGS: &str = "FRAGS";
static SEPARATOR: &str = "SEPARATOR";
static HIGHLIGHT: &str = "HIGHLIGHT";
static TAGS: &str = "TAGS";
static SLOP: &str = "SLOP";
static TIMEOUT: &str = "TIMEOUT";
static INORDER: &str = "INORDER";
static LANGUAGE: &str = "LANGUAGE";
static EXPANDER: &str = "EXPANDER";
static SCORER: &str = "SCORER";
static EXPLAINSCORE: &str = "EXPLAINSCORE";
static SORTBY: &str = "SORTBY";
static PARAMS: &str = "PARAMS";
static WITHCOUNT: &str = "WITHCOUNT";
static LOAD: &str = "LOAD";
static WITHCURSOR: &str = "WITHCURSOR";
static MAXIDLE: &str = "MAXIDLE";
static APPLY: &str = "APPLY";
static GROUPBY: &str = "GROUPBY";
static REDUCE: &str = "REDUCE";
static ON: &str = "ON";
static HASH: &str = "HASH";
static JSON: &str = "JSON";
static PREFIX: &str = "PREFIX";
static LANGUAGE_FIELD: &str = "LANGUAGE_FIELD";
static SCORE: &str = "SCORE";
static SCORE_FIELD: &str = "SCORE_FIELD";
static PAYLOAD_FIELD: &str = "PAYLOAD_FIELD";
static MAXTEXTFIELDS: &str = "MAXTEXTFIELDS";
static TEMPORARY: &str = "TEMPORARY";
static NOOFFSETS: &str = "NOOFFSETS";
static NOHL: &str = "NOHL";
static NOFIELDS: &str = "NOFIELDS";
static NOFREQS: &str = "NOFREQS";
static STOPWORDS: &str = "STOPWORDS";
static SCHEMA: &str = "SCHEMA";
static ADD: &str = "ADD";
static SORTABLE: &str = "SORTABLE";
static UNF: &str = "UNF";
static NOINDEX: &str = "NOINDEX";
static NOSTEM: &str = "NOSTEM";
static PHONETIC: &str = "PHONETIC";
static WEIGHT: &str = "WEIGHT";
static CASESENSITIVE: &str = "CASESENSITIVE";
static WITHSUFFIXTRIE: &str = "WITHSUFFIXTRIE";
static TEXT: &str = "TEXT";
static TAG: &str = "TAG";
static NUMERIC: &str = "NUMERIC";
static GEO: &str = "GEO";
static VECTOR: &str = "VECTOR";
static GEOSHAPE: &str = "GEOSHAPE";

fn gen_aggregate_op(args: &mut Vec<Value>, operation: AggregateOperation) -> Result<(), Error> {
  match operation {
    AggregateOperation::Filter { expression } => {
      args.extend([static_val!(FILTER), expression.into()]);
    },
    AggregateOperation::Limit { offset, num } => {
      args.extend([static_val!(LIMIT), offset.try_into()?, num.try_into()?]);
    },
    AggregateOperation::Apply { expression, name } => {
      args.extend([static_val!(APPLY), expression.into(), static_val!(AS), name.into()]);
    },
    AggregateOperation::SortBy { properties, max } => {
      args.extend([static_val!(SORTBY), (properties.len() * 2).try_into()?]);
      for (property, order) in properties.into_iter() {
        args.extend([property.into(), order.to_str().into()]);
      }
      if let Some(max) = max {
        args.extend([static_val!(MAX), max.try_into()?]);
      }
    },
    AggregateOperation::GroupBy { fields, reducers } => {
      args.extend([static_val!(GROUPBY), fields.len().try_into()?]);
      args.extend(fields.into_iter().map(|f| f.into()));

      for reducer in reducers.into_iter() {
        args.extend([
          static_val!(REDUCE),
          static_val!(reducer.func.to_str()),
          reducer.args.len().try_into()?,
        ]);
        args.extend(reducer.args.into_iter().map(|a| a.into()));
        if let Some(name) = reducer.name {
          args.extend([static_val!(AS), name.into()]);
        }
      }
    },
  };

  Ok(())
}

fn gen_aggregate_options(args: &mut Vec<Value>, options: FtAggregateOptions) -> Result<(), Error> {
  if options.verbatim {
    args.push(static_val!(VERBATIM));
  }
  if let Some(load) = options.load {
    match load {
      Load::All => {
        args.extend([static_val!(LOAD), static_val!("*")]);
      },
      Load::Some(fields) => {
        if !fields.is_empty() {
          args.extend([static_val!(LOAD), fields.len().try_into()?]);
          for field in fields.into_iter() {
            args.push(field.identifier.into());
            if let Some(property) = field.property {
              args.extend([static_val!(AS), property.into()]);
            }
          }
        }
      },
    }
  }
  if let Some(timeout) = options.timeout {
    args.extend([static_val!(TIMEOUT), timeout.into()]);
  }
  for operation in options.pipeline.into_iter() {
    gen_aggregate_op(args, operation)?;
  }
  if let Some(cursor) = options.cursor {
    args.push(static_val!(WITHCURSOR));
    if let Some(count) = cursor.count {
      args.extend([static_val!(COUNT), count.try_into()?]);
    }
    if let Some(idle) = cursor.max_idle {
      args.extend([static_val!(MAXIDLE), idle.try_into()?]);
    }
  }
  if !options.params.is_empty() {
    args.extend([static_val!(PARAMS), options.params.len().try_into()?]);
    for param in options.params.into_iter() {
      args.extend([param.name.into(), param.value.into()]);
    }
  }
  if let Some(dialect) = options.dialect {
    args.extend([static_val!(DIALECT), dialect.into()]);
  }

  Ok(())
}

fn gen_search_options(args: &mut Vec<Value>, options: FtSearchOptions) -> Result<(), Error> {
  if options.nocontent {
    args.push(static_val!(NOCONTENT));
  }
  if options.verbatim {
    args.push(static_val!(VERBATIM));
  }
  if options.nostopwords {
    args.push(static_val!(NOSTOPWORDS));
  }
  if options.withscores {
    args.push(static_val!(WITHSCORES));
  }
  if options.withpayloads {
    args.push(static_val!(WITHPAYLOADS));
  }
  if options.withsortkeys {
    args.push(static_val!(WITHSORTKEYS));
  }
  for filter in options.filters.into_iter() {
    args.extend([
      static_val!(FILTER),
      filter.attribute.into(),
      filter.min.into_value()?,
      filter.max.into_value()?,
    ]);
  }
  for geo_filter in options.geofilters.into_iter() {
    args.extend([
      static_val!(GEOFILTER),
      geo_filter.attribute.into(),
      geo_filter.position.longitude.try_into()?,
      geo_filter.position.latitude.try_into()?,
      geo_filter.radius,
      geo_filter.units.to_str().into(),
    ]);
  }
  if !options.inkeys.is_empty() {
    args.push(static_val!(INKEYS));
    args.push(options.inkeys.len().try_into()?);
    args.extend(options.inkeys.into_iter().map(|k| k.into()));
  }
  if !options.infields.is_empty() {
    args.push(static_val!(INFIELDS));
    args.push(options.infields.len().try_into()?);
    args.extend(options.infields.into_iter().map(|s| s.into()));
  }
  if !options.r#return.is_empty() {
    args.extend([static_val!(_RETURN), options.r#return.len().try_into()?]);
    for field in options.r#return.into_iter() {
      args.push(field.identifier.into());
      if let Some(property) = field.property {
        args.push(static_val!(AS));
        args.push(property.into());
      }
    }
  }
  if let Some(summarize) = options.summarize {
    args.push(static_val!(SUMMARIZE));
    if !summarize.fields.is_empty() {
      args.push(static_val!(FIELDS));
      args.push(summarize.fields.len().try_into()?);
      args.extend(summarize.fields.into_iter().map(|s| s.into()));
    }
    if let Some(frags) = summarize.frags {
      args.push(static_val!(FRAGS));
      args.push(frags.try_into()?);
    }
    if let Some(len) = summarize.len {
      args.push(static_val!(LEN));
      args.push(len.try_into()?);
    }
    if let Some(separator) = summarize.separator {
      args.push(static_val!(SEPARATOR));
      args.push(separator.into());
    }
  }
  if let Some(highlight) = options.highlight {
    args.push(static_val!(HIGHLIGHT));
    if !highlight.fields.is_empty() {
      args.push(static_val!(FIELDS));
      args.push(highlight.fields.len().try_into()?);
      args.extend(highlight.fields.into_iter().map(|s| s.into()));
    }
    if let Some((open, close)) = highlight.tags {
      args.extend([static_val!(TAGS), open.into(), close.into()]);
    }
  }
  if let Some(slop) = options.slop {
    args.extend([static_val!(SLOP), slop.into()]);
  }
  if let Some(timeout) = options.timeout {
    args.extend([static_val!(TIMEOUT), timeout.into()]);
  }
  if options.inorder {
    args.push(static_val!(INORDER));
  }
  if let Some(language) = options.language {
    args.extend([static_val!(LANGUAGE), language.into()]);
  }
  if let Some(expander) = options.expander {
    args.extend([static_val!(EXPANDER), expander.into()]);
  }
  if let Some(scorer) = options.scorer {
    args.extend([static_val!(SCORER), scorer.into()]);
  }
  if options.explainscore {
    args.push(static_val!(EXPLAINSCORE));
  }
  if let Some(payload) = options.payload {
    args.extend([static_val!(PAYLOAD), Value::Bytes(payload)]);
  }
  if let Some(sort) = options.sortby {
    args.push(static_val!(SORTBY));
    args.push(sort.attribute.into());
    if let Some(order) = sort.order {
      args.push(order.to_str().into());
    }
    if sort.withcount {
      args.push(static_val!(WITHCOUNT));
    }
  }
  if let Some((offset, count)) = options.limit {
    args.extend([static_val!(LIMIT), offset.into(), count.into()]);
  }
  if !options.params.is_empty() {
    args.push(static_val!(PARAMS));
    args.push(options.params.len().try_into()?);
    for param in options.params.into_iter() {
      args.extend([param.name.into(), param.value.into()]);
    }
  }
  if let Some(dialect) = options.dialect {
    args.extend([static_val!(DIALECT), dialect.into()]);
  }

  Ok(())
}

fn gen_schema_kind(args: &mut Vec<Value>, kind: SearchSchemaKind) -> Result<(), Error> {
  match kind {
    SearchSchemaKind::Custom { name, arguments } => {
      args.push(name.into());
      args.extend(arguments);
    },
    SearchSchemaKind::GeoShape { noindex } => {
      args.push(static_val!(GEOSHAPE));
      if noindex {
        args.push(static_val!(NOINDEX));
      }
    },
    SearchSchemaKind::Vector { noindex } => {
      args.push(static_val!(VECTOR));
      if noindex {
        args.push(static_val!(NOINDEX));
      }
    },
    SearchSchemaKind::Geo { sortable, unf, noindex } => {
      args.push(static_val!(GEO));
      if sortable {
        args.push(static_val!(SORTABLE));
      }
      if unf {
        args.push(static_val!(UNF));
      }
      if noindex {
        args.push(static_val!(NOINDEX));
      }
    },
    SearchSchemaKind::Numeric { sortable, unf, noindex } => {
      args.push(static_val!(NUMERIC));
      if sortable {
        args.push(static_val!(SORTABLE));
      }
      if unf {
        args.push(static_val!(UNF));
      }
      if noindex {
        args.push(static_val!(NOINDEX));
      }
    },
    SearchSchemaKind::Tag {
      sortable,
      unf,
      separator,
      casesensitive,
      withsuffixtrie,
      noindex,
    } => {
      args.push(static_val!(TAG));
      if sortable {
        args.push(static_val!(SORTABLE));
      }
      if unf {
        args.push(static_val!(UNF));
      }
      if let Some(separator) = separator {
        args.extend([static_val!(SEPARATOR), separator.to_string().into()]);
      }
      if casesensitive {
        args.push(static_val!(CASESENSITIVE));
      }
      if withsuffixtrie {
        args.push(static_val!(WITHSUFFIXTRIE));
      }
      if noindex {
        args.push(static_val!(NOINDEX));
      }
    },
    SearchSchemaKind::Text {
      sortable,
      unf,
      nostem,
      phonetic,
      weight,
      withsuffixtrie,
      noindex,
    } => {
      args.push(static_val!(TEXT));
      if sortable {
        args.push(static_val!(SORTABLE));
      }
      if unf {
        args.push(static_val!(UNF));
      }
      if nostem {
        args.push(static_val!(NOSTEM));
      }
      if let Some(matcher) = phonetic {
        args.extend([static_val!(PHONETIC), matcher.into()]);
      }
      if let Some(weight) = weight {
        args.extend([static_val!(WEIGHT), weight.into()]);
      }
      if withsuffixtrie {
        args.push(static_val!(WITHSUFFIXTRIE));
      }
      if noindex {
        args.push(static_val!(NOINDEX));
      }
    },
  };

  Ok(())
}

fn gen_alter_options(args: &mut Vec<Value>, options: FtAlterOptions) -> Result<(), Error> {
  if options.skipinitialscan {
    args.push(static_val!(SKIPINITIALSCAN));
  }
  args.extend([static_val!(SCHEMA), static_val!(ADD), options.attribute.into()]);
  gen_schema_kind(args, options.options)?;

  Ok(())
}

fn gen_create_options(args: &mut Vec<Value>, options: FtCreateOptions) -> Result<(), Error> {
  if let Some(kind) = options.on {
    args.extend([static_val!(ON), kind.to_str().into()]);
  }
  if !options.prefixes.is_empty() {
    args.extend([static_val!(PREFIX), options.prefixes.len().try_into()?]);
    args.extend(options.prefixes.into_iter().map(|s| s.into()));
  }
  if let Some(filter) = options.filter {
    args.extend([static_val!(FILTER), filter.into()]);
  }
  if let Some(language) = options.language {
    args.extend([static_val!(LANGUAGE), language.into()]);
  }
  if let Some(language_field) = options.language_field {
    args.extend([static_val!(LANGUAGE_FIELD), language_field.into()]);
  }
  if let Some(score) = options.score {
    args.extend([static_val!(SCORE), score.try_into()?]);
  }
  if let Some(score_field) = options.score_field {
    args.extend([static_val!(SCORE_FIELD), score_field.try_into()?]);
  }
  if let Some(payload_field) = options.payload_field {
    args.extend([static_val!(PAYLOAD_FIELD), payload_field.into()]);
  }
  if options.maxtextfields {
    args.push(static_val!(MAXTEXTFIELDS));
  }
  if let Some(temporary) = options.temporary {
    args.extend([static_val!(TEMPORARY), temporary.try_into()?]);
  }
  if options.nooffsets {
    args.push(static_val!(NOOFFSETS));
  }
  if options.nohl {
    args.push(static_val!(NOHL));
  }
  if options.nofields {
    args.push(static_val!(NOFIELDS));
  }
  if options.nofreqs {
    args.push(static_val!(NOFREQS));
  }
  if !options.stopwords.is_empty() {
    args.extend([static_val!(STOPWORDS), options.stopwords.len().try_into()?]);
    args.extend(options.stopwords.into_iter().map(|s| s.into()));
  }
  if options.skipinitialscan {
    args.push(static_val!(SKIPINITIALSCAN));
  }

  Ok(())
}

// does not include the prefix SCHEMA
fn gen_schema_args(args: &mut Vec<Value>, options: SearchSchema) -> Result<(), Error> {
  args.push(options.field_name.into());
  if let Some(alias) = options.alias {
    args.extend([static_val!(AS), alias.into()]);
  }
  gen_schema_kind(args, options.kind)?;

  Ok(())
}

pub async fn ft_list<C: ClientLike>(client: &C) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::FtList, vec![]).await
}

pub async fn ft_aggregate<C: ClientLike>(
  client: &C,
  index: Str,
  query: Str,
  options: FtAggregateOptions,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2 + options.num_args());
    args.push(index.into());
    args.push(query.into());
    gen_aggregate_options(&mut args, options)?;

    Ok((CommandKind::FtAggregate, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ft_search<C: ClientLike>(
  client: &C,
  index: Str,
  query: Str,
  options: FtSearchOptions,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(2 + options.num_args());
    args.extend([index.into(), query.into()]);
    gen_search_options(&mut args, options)?;

    Ok((CommandKind::FtSearch, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ft_create<C: ClientLike>(
  client: &C,
  index: Str,
  options: FtCreateOptions,
  schema: Vec<SearchSchema>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let schema_num_args = schema.iter().fold(0, |m, s| m + s.num_args());
    let mut args = Vec::with_capacity(2 + options.num_args() + schema_num_args);
    args.push(index.into());
    gen_create_options(&mut args, options)?;

    args.push(static_val!(SCHEMA));
    for schema in schema.into_iter() {
      gen_schema_args(&mut args, schema)?;
    }

    Ok((CommandKind::FtCreate, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ft_alter<C: ClientLike>(client: &C, index: Str, options: FtAlterOptions) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(1 + options.num_args());
    args.push(index.into());
    gen_alter_options(&mut args, options)?;

    Ok((CommandKind::FtAlter, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ft_aliasadd<C: ClientLike>(client: &C, alias: Str, index: Str) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::FtAliasAdd, vec![alias.into(), index.into()]).await
}

pub async fn ft_aliasdel<C: ClientLike>(client: &C, alias: Str) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::FtAliasDel, vec![alias.into()]).await
}

pub async fn ft_aliasupdate<C: ClientLike>(client: &C, alias: Str, index: Str) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::FtAliasUpdate, vec![alias.into(), index.into()]).await
}

pub async fn ft_config_get<C: ClientLike>(client: &C, option: Str) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::FtConfigGet, vec![option.into()]).await
}

pub async fn ft_config_set<C: ClientLike>(client: &C, option: Str, value: Value) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::FtConfigSet, vec![option.into(), value]).await
}

pub async fn ft_cursor_del<C: ClientLike>(client: &C, index: Str, cursor: Value) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::FtCursorDel, vec![index.into(), cursor]).await
}

pub async fn ft_cursor_read<C: ClientLike>(
  client: &C,
  index: Str,
  cursor: Value,
  count: Option<u64>,
) -> Result<Value, Error> {
  let args = if let Some(count) = count {
    vec![index.into(), cursor, static_val!(COUNT), count.try_into()?]
  } else {
    vec![index.into(), cursor]
  };

  args_values_cmd(client, CommandKind::FtCursorRead, args).await
}

pub async fn ft_dictadd<C: ClientLike>(client: &C, dict: Str, terms: MultipleStrings) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(terms.len() + 1);
    args.push(dict.into());
    for term in terms.inner().into_iter() {
      args.push(term.into());
    }

    Ok((CommandKind::FtDictAdd, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ft_dictdel<C: ClientLike>(client: &C, dict: Str, terms: MultipleStrings) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(terms.len() + 1);
    args.push(dict.into());
    for term in terms.inner().into_iter() {
      args.push(term.into());
    }

    Ok((CommandKind::FtDictDel, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ft_dictdump<C: ClientLike>(client: &C, dict: Str) -> Result<Value, Error> {
  one_arg_values_cmd(client, CommandKind::FtDictDump, dict.into()).await
}

pub async fn ft_dropindex<C: ClientLike>(client: &C, index: Str, dd: bool) -> Result<Value, Error> {
  let args = if dd {
    vec![index.into(), static_val!(DD)]
  } else {
    vec![index.into()]
  };

  args_values_cmd(client, CommandKind::FtDropIndex, args).await
}

pub async fn ft_explain<C: ClientLike>(
  client: &C,
  index: Str,
  query: Str,
  dialect: Option<i64>,
) -> Result<Value, Error> {
  let args = if let Some(dialect) = dialect {
    vec![index.into(), query.into(), static_val!(DIALECT), dialect.into()]
  } else {
    vec![index.into(), query.into()]
  };

  args_values_cmd(client, CommandKind::FtExplain, args).await
}

pub async fn ft_info<C: ClientLike>(client: &C, index: Str) -> Result<Value, Error> {
  one_arg_values_cmd(client, CommandKind::FtInfo, index.into()).await
}

pub async fn ft_spellcheck<C: ClientLike>(
  client: &C,
  index: Str,
  query: Str,
  distance: Option<u8>,
  terms: Option<SpellcheckTerms>,
  dialect: Option<i64>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let terms_len = terms.as_ref().map(|t| t.num_args()).unwrap_or(0);
    let mut args = Vec::with_capacity(9 + terms_len);
    args.push(index.into());
    args.push(query.into());

    if let Some(distance) = distance {
      args.push(static_val!(DISTANCE));
      args.push((distance as i64).into());
    }
    if let Some(terms) = terms {
      args.push(static_val!(TERMS));
      let (dictionary, terms) = match terms {
        SpellcheckTerms::Include { dictionary, terms } => {
          args.push(static_val!(INCLUDE));
          (dictionary, terms)
        },
        SpellcheckTerms::Exclude { dictionary, terms } => {
          args.push(static_val!(EXCLUDE));
          (dictionary, terms)
        },
      };

      args.push(dictionary.into());
      for term in terms.into_iter() {
        args.push(term.into());
      }
    }
    if let Some(dialect) = dialect {
      args.extend([static_val!(DIALECT), dialect.into()]);
    }

    Ok((CommandKind::FtSpellCheck, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ft_sugadd<C: ClientLike>(
  client: &C,
  key: Key,
  string: Str,
  score: f64,
  incr: bool,
  payload: Option<Bytes>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(6);
    args.extend([key.into(), string.into(), score.try_into()?]);

    if incr {
      args.push(static_val!(INCR));
    }
    if let Some(payload) = payload {
      args.extend([static_val!(PAYLOAD), Value::Bytes(payload)]);
    }

    Ok((CommandKind::FtSugAdd, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ft_sugdel<C: ClientLike>(client: &C, key: Key, string: Str) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::FtSugDel, vec![key.into(), string.into()]).await
}

pub async fn ft_sugget<C: ClientLike>(
  client: &C,
  key: Key,
  prefix: Str,
  fuzzy: bool,
  withscores: bool,
  withpayloads: bool,
  max: Option<u64>,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(7);
    args.push(key.into());
    args.push(prefix.into());
    if fuzzy {
      args.push(static_val!(FUZZY));
    }
    if withscores {
      args.push(static_val!(WITHSCORES));
    }
    if withpayloads {
      args.push(static_val!(WITHPAYLOADS));
    }
    if let Some(max) = max {
      args.extend([static_val!(MAX), max.try_into()?]);
    }

    Ok((CommandKind::FtSugGet, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ft_suglen<C: ClientLike>(client: &C, key: Key) -> Result<Value, Error> {
  one_arg_values_cmd(client, CommandKind::FtSugLen, key.into()).await
}

pub async fn ft_syndump<C: ClientLike>(client: &C, index: Str) -> Result<Value, Error> {
  one_arg_values_cmd(client, CommandKind::FtSynDump, index.into()).await
}

pub async fn ft_synupdate<C: ClientLike>(
  client: &C,
  index: Str,
  synonym_group_id: Str,
  skipinitialscan: bool,
  terms: MultipleStrings,
) -> Result<Value, Error> {
  let frame = utils::request_response(client, move || {
    let mut args = Vec::with_capacity(3 + terms.len());
    args.push(index.into());
    args.push(synonym_group_id.into());
    if skipinitialscan {
      args.push(static_val!(SKIPINITIALSCAN));
    }
    for term in terms.inner().into_iter() {
      args.push(term.into());
    }

    Ok((CommandKind::FtSynUpdate, args))
  })
  .await?;

  protocol_utils::frame_to_results(frame)
}

pub async fn ft_tagvals<C: ClientLike>(client: &C, index: Str, field_name: Str) -> Result<Value, Error> {
  args_values_cmd(client, CommandKind::FtTagVals, vec![index.into(), field_name.into()]).await
}
