use crate::{error::Error, protocol::utils as protocol_utils, types::Value, utils};
use bytes_utils::Str;
use std::{
  collections::VecDeque,
  convert::{TryFrom, TryInto},
};

/// A struct describing the longitude and latitude coordinates of a GEO command.
#[derive(Clone, Debug)]
pub struct GeoPosition {
  pub longitude: f64,
  pub latitude:  f64,
}

impl PartialEq for GeoPosition {
  fn eq(&self, other: &Self) -> bool {
    utils::f64_eq(self.longitude, other.longitude) && utils::f64_eq(self.latitude, other.latitude)
  }
}

impl Eq for GeoPosition {}

impl From<(f64, f64)> for GeoPosition {
  fn from(d: (f64, f64)) -> Self {
    GeoPosition {
      longitude: d.0,
      latitude:  d.1,
    }
  }
}

impl TryFrom<Value> for GeoPosition {
  type Error = Error;

  fn try_from(value: Value) -> Result<Self, Self::Error> {
    let (longitude, latitude): (f64, f64) = value.convert()?;
    Ok(GeoPosition { longitude, latitude })
  }
}

/// Units for the GEO DIST command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GeoUnit {
  Meters,
  Kilometers,
  Miles,
  Feet,
}

impl GeoUnit {
  pub(crate) fn to_str(&self) -> Str {
    utils::static_str(match *self {
      GeoUnit::Meters => "m",
      GeoUnit::Kilometers => "km",
      GeoUnit::Feet => "ft",
      GeoUnit::Miles => "mi",
    })
  }
}

/// A struct describing the value inside a GEO data structure.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GeoValue {
  pub coordinates: GeoPosition,
  pub member:      Value,
}

impl<T> TryFrom<(f64, f64, T)> for GeoValue
where
  T: TryInto<Value>,
  T::Error: Into<Error>,
{
  type Error = Error;

  fn try_from(v: (f64, f64, T)) -> Result<Self, Self::Error> {
    Ok(GeoValue {
      coordinates: GeoPosition {
        longitude: v.0,
        latitude:  v.1,
      },
      member:      utils::try_into(v.2)?,
    })
  }
}

/// A convenience struct for commands that take one or more GEO values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MultipleGeoValues {
  inner: Vec<GeoValue>,
}

impl MultipleGeoValues {
  pub fn len(&self) -> usize {
    self.inner.len()
  }

  pub fn inner(self) -> Vec<GeoValue> {
    self.inner
  }
}

impl From<GeoValue> for MultipleGeoValues {
  fn from(d: GeoValue) -> Self {
    MultipleGeoValues { inner: vec![d] }
  }
}

impl From<Vec<GeoValue>> for MultipleGeoValues {
  fn from(d: Vec<GeoValue>) -> Self {
    MultipleGeoValues { inner: d }
  }
}

impl From<VecDeque<GeoValue>> for MultipleGeoValues {
  fn from(d: VecDeque<GeoValue>) -> Self {
    MultipleGeoValues {
      inner: d.into_iter().collect(),
    }
  }
}

/// A typed struct representing the full output of the GEORADIUS (or similar) command.
#[derive(Clone, Debug)]
pub struct GeoRadiusInfo {
  pub member:   Value,
  pub position: Option<GeoPosition>,
  pub distance: Option<f64>,
  pub hash:     Option<i64>,
}

impl Default for GeoRadiusInfo {
  fn default() -> Self {
    GeoRadiusInfo {
      member:   Value::Null,
      position: None,
      distance: None,
      hash:     None,
    }
  }
}

impl PartialEq for GeoRadiusInfo {
  fn eq(&self, other: &Self) -> bool {
    self.member == other.member
      && self.position == other.position
      && self.hash == other.hash
      && utils::f64_opt_eq(&self.distance, &other.distance)
  }
}

impl Eq for GeoRadiusInfo {}

impl GeoRadiusInfo {
  /// Parse the value with context from the calling command.
  pub fn from_value(value: Value, withcoord: bool, withdist: bool, withhash: bool) -> Result<Self, Error> {
    if let Value::Array(mut data) = value {
      let mut out = GeoRadiusInfo::default();
      data.reverse();

      if withcoord && withdist && withhash {
        // 4 elements: member, dist, hash, position
        protocol_utils::assert_array_len(&data, 4)?;

        out.member = data.pop().unwrap();
        out.distance = data.pop().unwrap().convert()?;
        out.hash = data.pop().unwrap().convert()?;
        out.position = data.pop().unwrap().convert()?;
      } else if withcoord && withdist {
        // 3 elements: member, dist, position
        protocol_utils::assert_array_len(&data, 3)?;

        out.member = data.pop().unwrap();
        out.distance = data.pop().unwrap().convert()?;
        out.position = data.pop().unwrap().convert()?;
      } else if withcoord && withhash {
        // 3 elements: member, hash, position
        protocol_utils::assert_array_len(&data, 3)?;

        out.member = data.pop().unwrap();
        out.hash = data.pop().unwrap().convert()?;
        out.position = data.pop().unwrap().convert()?;
      } else if withdist && withhash {
        // 3 elements: member, dist, hash
        protocol_utils::assert_array_len(&data, 3)?;

        out.member = data.pop().unwrap();
        out.distance = data.pop().unwrap().convert()?;
        out.hash = data.pop().unwrap().convert()?;
      } else if withcoord {
        // 2 elements: member, position
        protocol_utils::assert_array_len(&data, 2)?;

        out.member = data.pop().unwrap();
        out.position = data.pop().unwrap().convert()?;
      } else if withdist {
        // 2 elements: member, dist
        protocol_utils::assert_array_len(&data, 2)?;

        out.member = data.pop().unwrap();
        out.distance = data.pop().unwrap().convert()?;
      } else if withhash {
        // 2 elements: member, hash
        protocol_utils::assert_array_len(&data, 2)?;

        out.member = data.pop().unwrap();
        out.hash = data.pop().unwrap().convert()?;
      }

      Ok(out)
    } else {
      Ok(GeoRadiusInfo {
        member: value,
        ..Default::default()
      })
    }
  }
}
