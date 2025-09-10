use crate::{
  commands,
  interfaces::{ClientLike, FredResult},
  prelude::Error,
  types::{
    redisearch::{
      FtAggregateOptions,
      FtAlterOptions,
      FtCreateOptions,
      FtSearchOptions,
      SearchSchema,
      SpellcheckTerms,
    },
    FromValue,
    Key,
    MultipleStrings,
    Value,
  },
};
use bytes::Bytes;
use bytes_utils::Str;
use fred_macros::rm_send_if;
use std::future::Future;

/// A [RediSearch](https://github.com/RediSearch/RediSearch) interface.
#[cfg_attr(docsrs, doc(cfg(feature = "i-redisearch")))]
#[rm_send_if(feature = "glommio")]
pub trait RediSearchInterface: ClientLike + Sized {
  /// Returns a list of all existing indexes.
  ///
  /// <https://redis.io/docs/latest/commands/ft._list/>
  fn ft_list<R>(&self) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
  {
    async move { commands::redisearch::ft_list(self).await?.convert() }
  }

  /// Run a search query on an index, and perform aggregate transformations on the results.
  ///
  /// <https://redis.io/docs/latest/commands/ft.aggregate/>
  fn ft_aggregate<R, I, Q>(
    &self,
    index: I,
    query: Q,
    options: FtAggregateOptions,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
    Q: Into<Str> + Send,
  {
    async move {
      into!(index, query);
      commands::redisearch::ft_aggregate(self, index, query, options)
        .await?
        .convert()
    }
  }

  /// Search the index with a textual query, returning either documents or just ids.
  ///
  /// <https://redis.io/docs/latest/commands/ft.search/>
  ///
  /// Note: `FT.SEARCH` uses a different format in RESP3 mode.
  fn ft_search<R, I, Q>(
    &self,
    index: I,
    query: Q,
    options: FtSearchOptions,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
    Q: Into<Str> + Send,
  {
    async move {
      into!(index, query);
      commands::redisearch::ft_search(self, index, query, options)
        .await?
        .convert()
    }
  }

  /// Create an index with the given specification.
  ///
  /// <https://redis.io/docs/latest/commands/ft.create/>
  fn ft_create<R, I>(
    &self,
    index: I,
    options: FtCreateOptions,
    schema: Vec<SearchSchema>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
  {
    async move {
      into!(index);
      commands::redisearch::ft_create(self, index, options, schema)
        .await?
        .convert()
    }
  }

  /// Add a new attribute to the index.
  ///
  /// <https://redis.io/docs/latest/commands/ft.alter/>
  fn ft_alter<R, I>(&self, index: I, options: FtAlterOptions) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
  {
    async move {
      into!(index);
      commands::redisearch::ft_alter(self, index, options).await?.convert()
    }
  }

  /// Add an alias to an index.
  ///
  /// <https://redis.io/docs/latest/commands/ft.aliasadd/>
  fn ft_aliasadd<R, A, I>(&self, alias: A, index: I) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    A: Into<Str> + Send,
    I: Into<Str> + Send,
  {
    async move {
      into!(alias, index);
      commands::redisearch::ft_aliasadd(self, alias, index).await?.convert()
    }
  }

  /// Remove an alias from an index.
  ///
  /// <https://redis.io/docs/latest/commands/ft.aliasdel/>
  fn ft_aliasdel<R, A>(&self, alias: A) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    A: Into<Str> + Send,
  {
    async move {
      into!(alias);
      commands::redisearch::ft_aliasdel(self, alias).await?.convert()
    }
  }

  /// Add an alias to an index. If the alias is already associated with another index, FT.ALIASUPDATE removes the
  /// alias association with the previous index.
  ///
  /// <https://redis.io/docs/latest/commands/ft.aliasupdate/>
  fn ft_aliasupdate<R, A, I>(&self, alias: A, index: I) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    A: Into<Str> + Send,
    I: Into<Str> + Send,
  {
    async move {
      into!(alias, index);
      commands::redisearch::ft_aliasupdate(self, alias, index)
        .await?
        .convert()
    }
  }

  /// Retrieve configuration options.
  ///
  /// <https://redis.io/docs/latest/commands/ft.config-get/>
  fn ft_config_get<R, S>(&self, option: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
  {
    async move {
      into!(option);
      commands::redisearch::ft_config_get(self, option).await?.convert()
    }
  }

  /// Set the value of a RediSearch configuration parameter.
  ///
  /// <https://redis.io/docs/latest/commands/ft.config-set/>
  fn ft_config_set<R, S, V>(&self, option: S, value: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    S: Into<Str> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(option);
      try_into!(value);
      commands::redisearch::ft_config_set(self, option, value)
        .await?
        .convert()
    }
  }

  /// Delete a cursor.
  ///
  /// <https://redis.io/docs/latest/commands/ft.cursor-del/>
  fn ft_cursor_del<R, I, C>(&self, index: I, cursor: C) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
    C: TryInto<Value> + Send,
    C::Error: Into<Error> + Send,
  {
    async move {
      into!(index);
      try_into!(cursor);
      commands::redisearch::ft_cursor_del(self, index, cursor)
        .await?
        .convert()
    }
  }

  /// Read next results from an existing cursor.
  ///
  /// <https://redis.io/docs/latest/commands/ft.cursor-read/>
  fn ft_cursor_read<R, I, C>(
    &self,
    index: I,
    cursor: C,
    count: Option<u64>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
    C: TryInto<Value> + Send,
    C::Error: Into<Error> + Send,
  {
    async move {
      into!(index);
      try_into!(cursor);
      commands::redisearch::ft_cursor_read(self, index, cursor, count)
        .await?
        .convert()
    }
  }

  /// Add terms to a dictionary.
  ///
  /// <https://redis.io/docs/latest/commands/ft.dictadd/>
  fn ft_dictadd<R, D, S>(&self, dict: D, terms: S) -> impl Future<Output = FredResult<R>>
  where
    R: FromValue,
    D: Into<Str> + Send,
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(dict, terms);
      commands::redisearch::ft_dictadd(self, dict, terms).await?.convert()
    }
  }

  /// Remove terms from a dictionary.
  ///
  /// <https://redis.io/docs/latest/commands/ft.dictdel/>
  fn ft_dictdel<R, D, S>(&self, dict: D, terms: S) -> impl Future<Output = FredResult<R>>
  where
    R: FromValue,
    D: Into<Str> + Send,
    S: Into<MultipleStrings> + Send,
  {
    async move {
      into!(dict, terms);
      commands::redisearch::ft_dictdel(self, dict, terms).await?.convert()
    }
  }

  /// Dump all terms in the given dictionary.
  ///
  /// <https://redis.io/docs/latest/commands/ft.dictdump/>
  fn ft_dictdump<R, D>(&self, dict: D) -> impl Future<Output = FredResult<R>>
  where
    R: FromValue,
    D: Into<Str> + Send,
  {
    async move {
      into!(dict);
      commands::redisearch::ft_dictdump(self, dict).await?.convert()
    }
  }

  /// Delete an index.
  ///
  /// <https://redis.io/docs/latest/commands/ft.dropindex/>
  fn ft_dropindex<R, I>(&self, index: I, dd: bool) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
  {
    async move {
      into!(index);
      commands::redisearch::ft_dropindex(self, index, dd).await?.convert()
    }
  }

  /// Return the execution plan for a complex query.
  ///
  /// <https://redis.io/docs/latest/commands/ft.explain/>
  fn ft_explain<R, I, Q>(
    &self,
    index: I,
    query: Q,
    dialect: Option<i64>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
    Q: Into<Str> + Send,
  {
    async move {
      into!(index, query);
      commands::redisearch::ft_explain(self, index, query, dialect)
        .await?
        .convert()
    }
  }

  /// Return information and statistics on the index.
  ///
  /// <https://redis.io/docs/latest/commands/ft.info/>
  fn ft_info<R, I>(&self, index: I) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
  {
    async move {
      into!(index);
      commands::redisearch::ft_info(self, index).await?.convert()
    }
  }

  /// Perform spelling correction on a query, returning suggestions for misspelled terms.
  ///
  /// <https://redis.io/docs/latest/commands/ft.spellcheck/>
  fn ft_spellcheck<R, I, Q>(
    &self,
    index: I,
    query: Q,
    distance: Option<u8>,
    terms: Option<SpellcheckTerms>,
    dialect: Option<i64>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
    Q: Into<Str> + Send,
  {
    async move {
      into!(index, query);
      commands::redisearch::ft_spellcheck(self, index, query, distance, terms, dialect)
        .await?
        .convert()
    }
  }

  /// Add a suggestion string to an auto-complete suggestion dictionary.
  ///
  /// <https://redis.io/docs/latest/commands/ft.sugadd/>
  fn ft_sugadd<R, K, S>(
    &self,
    key: K,
    string: S,
    score: f64,
    incr: bool,
    payload: Option<Bytes>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: Into<Str> + Send,
  {
    async move {
      into!(key, string);
      commands::redisearch::ft_sugadd(self, key, string, score, incr, payload)
        .await?
        .convert()
    }
  }

  /// Delete a string from a suggestion index.
  ///
  /// <https://redis.io/docs/latest/commands/ft.sugdel/>
  fn ft_sugdel<R, K, S>(&self, key: K, string: S) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: Into<Str> + Send,
  {
    async move {
      into!(key, string);
      commands::redisearch::ft_sugdel(self, key, string).await?.convert()
    }
  }

  /// Get completion suggestions for a prefix.
  ///
  /// <https://redis.io/docs/latest/commands/ft.sugget/>
  fn ft_sugget<R, K, P>(
    &self,
    key: K,
    prefix: P,
    fuzzy: bool,
    withscores: bool,
    withpayloads: bool,
    max: Option<u64>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<Str> + Send,
  {
    async move {
      into!(key, prefix);
      commands::redisearch::ft_sugget(self, key, prefix, fuzzy, withscores, withpayloads, max)
        .await?
        .convert()
    }
  }

  /// Get the size of an auto-complete suggestion dictionary.
  ///
  /// <https://redis.io/docs/latest/commands/ft.suglen/>
  fn ft_suglen<R, K>(&self, key: K) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::redisearch::ft_suglen(self, key).await?.convert()
    }
  }

  /// Dump the contents of a synonym group.
  ///
  /// <https://redis.io/docs/latest/commands/ft.syndump/>
  fn ft_syndump<R, I>(&self, index: I) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
  {
    async move {
      into!(index);
      commands::redisearch::ft_syndump(self, index).await?.convert()
    }
  }

  /// Update a synonym group.
  ///
  /// <https://redis.io/docs/latest/commands/ft.synupdate/>
  fn ft_synupdate<R, I, S, T>(
    &self,
    index: I,
    synonym_group_id: S,
    skipinitialscan: bool,
    terms: T,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
    S: Into<Str> + Send,
    T: Into<MultipleStrings> + Send,
  {
    async move {
      into!(index, synonym_group_id, terms);
      commands::redisearch::ft_synupdate(self, index, synonym_group_id, skipinitialscan, terms)
        .await?
        .convert()
    }
  }

  /// Return a distinct set of values indexed in a Tag field.
  ///
  /// <https://redis.io/docs/latest/commands/ft.tagvals/>
  fn ft_tagvals<R, I, F>(&self, index: I, field_name: F) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    I: Into<Str> + Send,
    F: Into<Str> + Send,
  {
    async move {
      into!(index, field_name);
      commands::redisearch::ft_tagvals(self, index, field_name)
        .await?
        .convert()
    }
  }
}
