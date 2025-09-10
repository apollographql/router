use crate::{
  commands,
  error::Error,
  interfaces::{ClientLike, FredResult},
  types::{
    geo::{GeoPosition, GeoUnit, MultipleGeoValues},
    Any,
    FromValue,
    Key,
    MultipleValues,
    SetOptions,
    SortOrder,
    Value,
  },
};
use fred_macros::rm_send_if;
use futures::Future;
use std::convert::TryInto;

/// Functions that implement the [geo](https://redis.io/commands#geo) interface.
#[rm_send_if(feature = "glommio")]
pub trait GeoInterface: ClientLike + Sized {
  /// Adds the specified geospatial items (longitude, latitude, name) to the specified key.
  ///
  /// <https://redis.io/commands/geoadd>
  fn geoadd<R, K, V>(
    &self,
    key: K,
    options: Option<SetOptions>,
    changed: bool,
    values: V,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: Into<MultipleGeoValues> + Send,
  {
    async move {
      into!(key, values);
      commands::geo::geoadd(self, key, options, changed, values)
        .await?
        .convert()
    }
  }

  /// Return valid Geohash strings representing the position of one or more elements in a sorted set value
  /// representing a geospatial index (where elements were added using GEOADD).
  ///
  /// <https://redis.io/commands/geohash>
  fn geohash<R, K, V>(&self, key: K, members: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(members);
      commands::geo::geohash(self, key, members).await?.convert()
    }
  }

  /// Return the positions (longitude,latitude) of all the specified members of the geospatial index represented by
  /// the sorted set at key.
  ///
  /// Callers can use [as_geo_position](crate::types::Value::as_geo_position) to lazily parse results as needed.
  ///
  /// <https://redis.io/commands/geopos>
  fn geopos<R, K, V>(&self, key: K, members: V) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<MultipleValues> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(members);
      commands::geo::geopos(self, key, members).await?.convert()
    }
  }

  /// Return the distance between two members in the geospatial index represented by the sorted set.
  ///
  /// <https://redis.io/commands/geodist>
  fn geodist<R, K, S, D>(
    &self,
    key: K,
    src: S,
    dest: D,
    unit: Option<GeoUnit>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    S: TryInto<Value> + Send,
    S::Error: Into<Error> + Send,
    D: TryInto<Value> + Send,
    D::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(src, dest);
      commands::geo::geodist(self, key, src, dest, unit).await?.convert()
    }
  }

  /// Return the members of a sorted set populated with geospatial information using GEOADD, which are within the
  /// borders of the area specified with the center location and the maximum distance from the center (the radius).
  ///
  /// <https://redis.io/commands/georadius>
  fn georadius<R, K, P>(
    &self,
    key: K,
    position: P,
    radius: f64,
    unit: GeoUnit,
    withcoord: bool,
    withdist: bool,
    withhash: bool,
    count: Option<(u64, Any)>,
    ord: Option<SortOrder>,
    store: Option<Key>,
    storedist: Option<Key>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    P: Into<GeoPosition> + Send,
  {
    async move {
      into!(key, position);
      commands::geo::georadius(
        self, key, position, radius, unit, withcoord, withdist, withhash, count, ord, store, storedist,
      )
      .await?
      .convert()
    }
  }

  /// This command is exactly like GEORADIUS with the sole difference that instead of taking, as the center of the
  /// area to query, a longitude and latitude value, it takes the name of a member already existing inside the
  /// geospatial index represented by the sorted set.
  ///
  /// <https://redis.io/commands/georadiusbymember>
  fn georadiusbymember<R, K, V>(
    &self,
    key: K,
    member: V,
    radius: f64,
    unit: GeoUnit,
    withcoord: bool,
    withdist: bool,
    withhash: bool,
    count: Option<(u64, Any)>,
    ord: Option<SortOrder>,
    store: Option<Key>,
    storedist: Option<Key>,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
    V: TryInto<Value> + Send,
    V::Error: Into<Error> + Send,
  {
    async move {
      into!(key);
      try_into!(member);
      commands::geo::georadiusbymember(
        self,
        key,
        to!(member)?,
        radius,
        unit,
        withcoord,
        withdist,
        withhash,
        count,
        ord,
        store,
        storedist,
      )
      .await?
      .convert()
    }
  }

  /// Return the members of a sorted set populated with geospatial information using GEOADD, which are within the
  /// borders of the area specified by a given shape.
  ///
  /// <https://redis.io/commands/geosearch>
  fn geosearch<R, K>(
    &self,
    key: K,
    from_member: Option<Value>,
    from_lonlat: Option<GeoPosition>,
    by_radius: Option<(f64, GeoUnit)>,
    by_box: Option<(f64, f64, GeoUnit)>,
    ord: Option<SortOrder>,
    count: Option<(u64, Any)>,
    withcoord: bool,
    withdist: bool,
    withhash: bool,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    K: Into<Key> + Send,
  {
    async move {
      into!(key);
      commands::geo::geosearch(
        self,
        key,
        from_member,
        from_lonlat,
        by_radius,
        by_box,
        ord,
        count,
        withcoord,
        withdist,
        withhash,
      )
      .await?
      .convert()
    }
  }

  /// This command is like GEOSEARCH, but stores the result in destination key. Returns the number of members added to
  /// the destination key.
  ///
  /// <https://redis.io/commands/geosearchstore>
  fn geosearchstore<R, D, S>(
    &self,
    dest: D,
    source: S,
    from_member: Option<Value>,
    from_lonlat: Option<GeoPosition>,
    by_radius: Option<(f64, GeoUnit)>,
    by_box: Option<(f64, f64, GeoUnit)>,
    ord: Option<SortOrder>,
    count: Option<(u64, Any)>,
    storedist: bool,
  ) -> impl Future<Output = FredResult<R>> + Send
  where
    R: FromValue,
    D: Into<Key> + Send,
    S: Into<Key> + Send,
  {
    async move {
      into!(dest, source);
      commands::geo::geosearchstore(
        self,
        dest,
        source,
        from_member,
        from_lonlat,
        by_radius,
        by_box,
        ord,
        count,
        storedist,
      )
      .await?
      .convert()
    }
  }
}
