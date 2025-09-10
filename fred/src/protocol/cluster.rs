use crate::{
  error::{Error, ErrorKind},
  modules::inner::ClientInner,
  protocol::types::{Server, SlotRange},
  runtime::RefCount,
  types::Value,
  utils,
};
use bytes_utils::Str;
use std::{collections::HashMap, net::IpAddr, str::FromStr};

#[cfg(any(
  feature = "enable-native-tls",
  feature = "enable-rustls",
  feature = "enable-rustls-ring"
))]
use crate::protocol::tls::TlsHostMapping;

fn parse_as_u16(value: Value) -> Result<u16, Error> {
  match value {
    Value::Integer(i) => {
      if i < 0 || i > u16::MAX as i64 {
        Err(Error::new(ErrorKind::Parse, "Invalid cluster slot integer."))
      } else {
        Ok(i as u16)
      }
    },
    Value::String(s) => s.parse::<u16>().map_err(|e| e.into()),
    _ => Err(Error::new(ErrorKind::Parse, "Could not parse value as cluster slot.")),
  }
}

fn is_ip_address(value: &Str) -> bool {
  IpAddr::from_str(value).is_ok()
}

fn check_metadata_hostname(data: &HashMap<Str, Str>) -> Option<&Str> {
  data
    .get(&utils::static_str("hostname"))
    .filter(|&hostname| !hostname.is_empty())
}

/// Find the correct hostname for the server, preferring hostnames over IP addresses for TLS purposes.
///
/// The format of `server` is `[<preferred host>|null, <port>, <id>, <metadata>]`. However, in Redis <=6 the
/// `metadata` value will not be present.
///
/// The implementation here does the following:
/// 1. If `server[0]` is a hostname then use that.
/// 2. If `server[0]` is an IP address, then check `server[3]` for a "hostname" metadata field and use that if found.
///    Otherwise use the IP address in `server[0]`.
/// 3. If `server[0]` is null, but `server[3]` has a "hostname" metadata field, then use the metadata field. Otherwise
///    use `default_host`.
///
/// The `default_host` is the host that returned the `CLUSTER SLOTS` response.
///
/// <https://redis.io/commands/cluster-slots/#nested-result-array>
fn parse_cluster_slot_hostname(server: &[Value], default_host: &Str) -> Result<Str, Error> {
  if server.is_empty() {
    return Err(Error::new(ErrorKind::Protocol, "Invalid CLUSTER SLOTS server block."));
  }
  let should_parse_metadata = server.len() >= 4 && !server[3].is_null() && server[3].array_len().unwrap_or(0) > 0;

  let metadata: HashMap<Str, Str> = if should_parse_metadata {
    // all the variants with data on the heap are ref counted (`Bytes`, `Str`, etc)
    server[3].clone().convert()?
  } else {
    HashMap::new()
  };
  if server[0].is_null() {
    // option 3
    Ok(check_metadata_hostname(&metadata).unwrap_or(default_host).clone())
  } else {
    let preferred_host = match server[0].clone().convert::<Str>() {
      Ok(host) => host,
      Err(_) => {
        return Err(Error::new(
          ErrorKind::Protocol,
          "Invalid CLUSTER SLOTS server block hostname.",
        ))
      },
    };

    if is_ip_address(&preferred_host) {
      // option 2
      Ok(check_metadata_hostname(&metadata).unwrap_or(&preferred_host).clone())
    } else {
      // option 1
      Ok(preferred_host)
    }
  }
}

/// Read the node block with format `<hostname>|null, <port>, <id>, [metadata]`
fn parse_node_block(data: &[Value], default_host: &Str) -> Option<(Str, u16, Str, Str)> {
  if data.len() < 3 {
    return None;
  }

  let hostname = match parse_cluster_slot_hostname(data, default_host) {
    Ok(host) => host,
    Err(_) => return None,
  };
  let port: u16 = match parse_as_u16(data[1].clone()) {
    Ok(port) => port,
    Err(_) => return None,
  };
  let primary = Str::from(format!("{}:{}", hostname, port));
  let id = data[2].as_bytes_str()?;

  Some((hostname, port, primary, id))
}

/// Parse the optional trailing replica nodes in each `CLUSTER SLOTS` slot range block.
#[cfg(feature = "replicas")]
fn parse_cluster_slot_replica_nodes(slot_range: Vec<Value>, default_host: &Str) -> Vec<Server> {
  slot_range
    .into_iter()
    .filter_map(|value| {
      let server_block: Vec<Value> = match value.convert() {
        Ok(v) => v,
        Err(_) => {
          warn!("Skip replica CLUSTER SLOTS block from {}", default_host);
          return None;
        },
      };

      let (host, port) = match parse_node_block(&server_block, default_host) {
        Some((h, p, _, _)) => (h, p),
        None => {
          warn!("Skip replica CLUSTER SLOTS block from {}", default_host);
          return None;
        },
      };

      Some(Server {
        host,
        port,
        #[cfg(any(
          feature = "enable-native-tls",
          feature = "enable-rustls",
          feature = "enable-rustls-ring"
        ))]
        tls_server_name: None,
      })
    })
    .collect()
}

/// Parse the cluster slot range and associated server blocks.
fn parse_cluster_slot_nodes(mut slot_range: Vec<Value>, default_host: &Str) -> Result<SlotRange, Error> {
  if slot_range.len() < 3 {
    return Err(Error::new(ErrorKind::Protocol, "Invalid CLUSTER SLOTS response."));
  }
  slot_range.reverse();
  // length checked above
  let start = parse_as_u16(slot_range.pop().unwrap())?;
  let end = parse_as_u16(slot_range.pop().unwrap())?;

  // the third value is the primary node, following values are optional replica nodes
  // length checked above. format is `<hostname>|null, <port>, <id>, [metadata]`
  let server_block: Vec<Value> = slot_range.pop().unwrap().convert()?;
  let (host, port, id) = match parse_node_block(&server_block, default_host) {
    Some((h, p, _, i)) => (h, p, i),
    None => {
      trace!("Failed to parse CLUSTER SLOTS response: {:?}", server_block);
      return Err(Error::new(ErrorKind::Cluster, "Invalid CLUSTER SLOTS response."));
    },
  };

  Ok(SlotRange {
    start,
    end,
    id,
    primary: Server {
      host,
      port,
      #[cfg(any(
        feature = "enable-native-tls",
        feature = "enable-rustls",
        feature = "enable-rustls-ring"
      ))]
      tls_server_name: None,
    },
    #[cfg(feature = "replicas")]
    replicas: parse_cluster_slot_replica_nodes(slot_range, default_host),
  })
}

/// Parse the entire CLUSTER SLOTS response with the provided `default_host` of the connection used to send the
/// command.
pub fn parse_cluster_slots(frame: Value, default_host: &Str) -> Result<Vec<SlotRange>, Error> {
  let slot_ranges: Vec<Vec<Value>> = frame.convert()?;
  let mut out: Vec<SlotRange> = Vec::with_capacity(slot_ranges.len());

  for slot_range in slot_ranges.into_iter() {
    out.push(parse_cluster_slot_nodes(slot_range, default_host)?);
  }

  out.shrink_to_fit();
  Ok(out)
}

#[cfg(any(
  feature = "enable-rustls",
  feature = "enable-native-tls",
  feature = "enable-rustls-ring"
))]
fn replace_tls_server_names(policy: &TlsHostMapping, ranges: &mut [SlotRange], default_host: &Str) {
  for slot_range in ranges.iter_mut() {
    slot_range.primary.set_tls_server_name(policy, default_host);

    #[cfg(feature = "replicas")]
    for server in slot_range.replicas.iter_mut() {
      server.set_tls_server_name(policy, default_host);
    }
  }
}

/// Modify the `CLUSTER SLOTS` command according to the hostname mapping policy in the `TlsHostMapping`.
#[cfg(any(
  feature = "enable-rustls",
  feature = "enable-native-tls",
  feature = "enable-rustls-ring"
))]
pub fn modify_cluster_slot_hostnames(inner: &RefCount<ClientInner>, ranges: &mut [SlotRange], default_host: &Str) {
  let policy = match inner.config.tls {
    Some(ref config) => &config.hostnames,
    None => {
      _trace!(inner, "Skip modifying TLS hostnames.");
      return;
    },
  };
  if *policy == TlsHostMapping::None {
    _trace!(inner, "Skip modifying TLS hostnames.");
    return;
  }

  replace_tls_server_names(policy, ranges, default_host);
}

#[cfg(not(any(
  feature = "enable-rustls",
  feature = "enable-native-tls",
  feature = "enable-rustls-ring"
)))]
pub fn modify_cluster_slot_hostnames(inner: &RefCount<ClientInner>, _: &mut Vec<SlotRange>, _: &Str) {
  _trace!(inner, "Skip modifying TLS hostnames.")
}

#[cfg(test)]
mod tests {
  use super::*;
  #[cfg(any(
    feature = "enable-native-tls",
    feature = "enable-rustls",
    feature = "enable-rustls-ring"
  ))]
  use crate::protocol::tls::{HostMapping, TlsHostMapping};
  use crate::protocol::types::SlotRange;

  #[cfg(any(
    feature = "enable-native-tls",
    feature = "enable-rustls",
    feature = "enable-rustls-ring"
  ))]
  #[derive(Debug)]
  struct FakeHostMapper;

  #[cfg(any(
    feature = "enable-native-tls",
    feature = "enable-rustls",
    feature = "enable-rustls-ring"
  ))]
  impl HostMapping for FakeHostMapper {
    fn map(&self, _: &IpAddr, _: &str) -> Option<String> {
      Some("foobarbaz".into())
    }
  }

  fn fake_cluster_slots_without_metadata() -> Value {
    let first_slot_range = Value::Array(vec![
      0.into(),
      5460.into(),
      Value::Array(vec![
        "127.0.0.1".into(),
        30001.into(),
        "09dbe9720cda62f7865eabc5fd8857c5d2678366".into(),
      ]),
      Value::Array(vec![
        "127.0.0.1".into(),
        30004.into(),
        "821d8ca00d7ccf931ed3ffc7e3db0599d2271abf".into(),
      ]),
    ]);
    let second_slot_range = Value::Array(vec![
      5461.into(),
      10922.into(),
      Value::Array(vec![
        "127.0.0.1".into(),
        30002.into(),
        "c9d93d9f2c0c524ff34cc11838c2003d8c29e013".into(),
      ]),
      Value::Array(vec![
        "127.0.0.1".into(),
        30005.into(),
        "faadb3eb99009de4ab72ad6b6ed87634c7ee410f".into(),
      ]),
    ]);
    let third_slot_range = Value::Array(vec![
      10923.into(),
      16383.into(),
      Value::Array(vec![
        "127.0.0.1".into(),
        30003.into(),
        "044ec91f325b7595e76dbcb18cc688b6a5b434a1".into(),
      ]),
      Value::Array(vec![
        "127.0.0.1".into(),
        30006.into(),
        "58e6e48d41228013e5d9c1c37c5060693925e97e".into(),
      ]),
    ]);

    Value::Array(vec![first_slot_range, second_slot_range, third_slot_range])
  }

  fn fake_cluster_slots_with_metadata() -> Value {
    let first_slot_range = Value::Array(vec![
      0.into(),
      5460.into(),
      Value::Array(vec![
        "127.0.0.1".into(),
        30001.into(),
        "09dbe9720cda62f7865eabc5fd8857c5d2678366".into(),
        Value::Array(vec!["hostname".into(), "host-1.redis.example.com".into()]),
      ]),
      Value::Array(vec![
        "127.0.0.1".into(),
        30004.into(),
        "821d8ca00d7ccf931ed3ffc7e3db0599d2271abf".into(),
        Value::Array(vec!["hostname".into(), "host-2.redis.example.com".into()]),
      ]),
    ]);
    let second_slot_range = Value::Array(vec![
      5461.into(),
      10922.into(),
      Value::Array(vec![
        "127.0.0.1".into(),
        30002.into(),
        "c9d93d9f2c0c524ff34cc11838c2003d8c29e013".into(),
        Value::Array(vec!["hostname".into(), "host-3.redis.example.com".into()]),
      ]),
      Value::Array(vec![
        "127.0.0.1".into(),
        30005.into(),
        "faadb3eb99009de4ab72ad6b6ed87634c7ee410f".into(),
        Value::Array(vec!["hostname".into(), "host-4.redis.example.com".into()]),
      ]),
    ]);
    let third_slot_range = Value::Array(vec![
      10923.into(),
      16383.into(),
      Value::Array(vec![
        "127.0.0.1".into(),
        30003.into(),
        "044ec91f325b7595e76dbcb18cc688b6a5b434a1".into(),
        Value::Array(vec!["hostname".into(), "host-5.redis.example.com".into()]),
      ]),
      Value::Array(vec![
        "127.0.0.1".into(),
        30006.into(),
        "58e6e48d41228013e5d9c1c37c5060693925e97e".into(),
        Value::Array(vec!["hostname".into(), "host-6.redis.example.com".into()]),
      ]),
    ]);

    Value::Array(vec![first_slot_range, second_slot_range, third_slot_range])
  }

  #[test]
  #[cfg(any(
    feature = "enable-native-tls",
    feature = "enable-rustls",
    feature = "enable-rustls-ring"
  ))]
  fn should_modify_cluster_slot_hostnames_default_host_without_metadata() {
    let policy = TlsHostMapping::DefaultHost;
    let fake_data = fake_cluster_slots_without_metadata();
    let mut ranges = parse_cluster_slots(fake_data, &Str::from("default-host")).unwrap();
    replace_tls_server_names(&policy, &mut ranges, &Str::from("default-host"));

    for slot_range in ranges.iter() {
      assert_ne!(slot_range.primary.host, "default-host");
      assert_eq!(slot_range.primary.tls_server_name, Some("default-host".into()));

      #[cfg(feature = "replicas")]
      for replica in slot_range.replicas.iter() {
        assert_ne!(replica.host, "default-host");
        assert_eq!(replica.tls_server_name, Some("default-host".into()));
      }
    }
  }

  #[test]
  #[cfg(any(
    feature = "enable-native-tls",
    feature = "enable-rustls",
    feature = "enable-rustls-ring"
  ))]
  fn should_not_modify_cluster_slot_hostnames_default_host_with_metadata() {
    let policy = TlsHostMapping::DefaultHost;
    let fake_data = fake_cluster_slots_with_metadata();
    let mut ranges = parse_cluster_slots(fake_data, &Str::from("default-host")).unwrap();
    replace_tls_server_names(&policy, &mut ranges, &Str::from("default-host"));

    for slot_range in ranges.iter() {
      assert_ne!(slot_range.primary.host, "default-host");
      // since there's a metadata hostname then expect that instead of the default host
      assert_ne!(slot_range.primary.tls_server_name, Some("default-host".into()));

      #[cfg(feature = "replicas")]
      for replica in slot_range.replicas.iter() {
        assert_ne!(replica.host, "default-host");
        assert_ne!(replica.tls_server_name, Some("default-host".into()));
      }
    }
  }

  #[test]
  #[cfg(any(
    feature = "enable-native-tls",
    feature = "enable-rustls",
    feature = "enable-rustls-ring"
  ))]
  fn should_modify_cluster_slot_hostnames_custom() {
    let policy = TlsHostMapping::Custom(RefCount::new(FakeHostMapper));
    let fake_data = fake_cluster_slots_without_metadata();
    let mut ranges = parse_cluster_slots(fake_data, &Str::from("default-host")).unwrap();
    replace_tls_server_names(&policy, &mut ranges, &Str::from("default-host"));

    for slot_range in ranges.iter() {
      assert_ne!(slot_range.primary.host, "default-host");
      assert_eq!(slot_range.primary.tls_server_name, Some("foobarbaz".into()));

      #[cfg(feature = "replicas")]
      for replica in slot_range.replicas.iter() {
        assert_ne!(replica.host, "default-host");
        assert_eq!(replica.tls_server_name, Some("foobarbaz".into()));
      }
    }
  }

  #[test]
  fn should_parse_cluster_slots_example_metadata_hostnames() {
    let input = fake_cluster_slots_with_metadata();

    let actual = parse_cluster_slots(input, &Str::from("bad-host")).expect("Failed to parse input");
    let expected = vec![
      SlotRange {
        start:                                 0,
        end:                                   5460,
        primary:                               Server {
          host:            "host-1.redis.example.com".into(),
          port:            30001,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "09dbe9720cda62f7865eabc5fd8857c5d2678366".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "host-2.redis.example.com".into(),
          port:            30004,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
      SlotRange {
        start:                                 5461,
        end:                                   10922,
        primary:                               Server {
          host:            "host-3.redis.example.com".into(),
          port:            30002,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "c9d93d9f2c0c524ff34cc11838c2003d8c29e013".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "host-4.redis.example.com".into(),
          port:            30005,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
      SlotRange {
        start:                                 10923,
        end:                                   16383,
        primary:                               Server {
          host:            "host-5.redis.example.com".into(),
          port:            30003,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "044ec91f325b7595e76dbcb18cc688b6a5b434a1".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "host-6.redis.example.com".into(),
          port:            30006,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
    ];
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_cluster_slots_example_no_metadata() {
    let input = fake_cluster_slots_without_metadata();

    let actual = parse_cluster_slots(input, &Str::from("bad-host")).expect("Failed to parse input");
    let expected = vec![
      SlotRange {
        start:                                 0,
        end:                                   5460,
        primary:                               Server {
          host:            "127.0.0.1".into(),
          port:            30001,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "09dbe9720cda62f7865eabc5fd8857c5d2678366".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "127.0.0.1".into(),
          port:            30004,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
      SlotRange {
        start:                                 5461,
        end:                                   10922,
        primary:                               Server {
          host:            "127.0.0.1".into(),
          port:            30002,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "c9d93d9f2c0c524ff34cc11838c2003d8c29e013".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "127.0.0.1".into(),
          port:            30005,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
      SlotRange {
        start:                                 10923,
        end:                                   16383,
        primary:                               Server {
          host:            "127.0.0.1".into(),
          port:            30003,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "044ec91f325b7595e76dbcb18cc688b6a5b434a1".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "127.0.0.1".into(),
          port:            30006,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
    ];
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_cluster_slots_example_empty_metadata() {
    let first_slot_range = Value::Array(vec![
      0.into(),
      5460.into(),
      Value::Array(vec![
        "127.0.0.1".into(),
        30001.into(),
        "09dbe9720cda62f7865eabc5fd8857c5d2678366".into(),
        Value::Array(vec![]),
      ]),
      Value::Array(vec![
        "127.0.0.1".into(),
        30004.into(),
        "821d8ca00d7ccf931ed3ffc7e3db0599d2271abf".into(),
        Value::Array(vec![]),
      ]),
    ]);
    let second_slot_range = Value::Array(vec![
      5461.into(),
      10922.into(),
      Value::Array(vec![
        "127.0.0.1".into(),
        30002.into(),
        "c9d93d9f2c0c524ff34cc11838c2003d8c29e013".into(),
        Value::Array(vec![]),
      ]),
      Value::Array(vec![
        "127.0.0.1".into(),
        30005.into(),
        "faadb3eb99009de4ab72ad6b6ed87634c7ee410f".into(),
        Value::Array(vec![]),
      ]),
    ]);
    let third_slot_range = Value::Array(vec![
      10923.into(),
      16383.into(),
      Value::Array(vec![
        "127.0.0.1".into(),
        30003.into(),
        "044ec91f325b7595e76dbcb18cc688b6a5b434a1".into(),
        Value::Array(vec![]),
      ]),
      Value::Array(vec![
        "127.0.0.1".into(),
        30006.into(),
        "58e6e48d41228013e5d9c1c37c5060693925e97e".into(),
        Value::Array(vec![]),
      ]),
    ]);
    let input = Value::Array(vec![first_slot_range, second_slot_range, third_slot_range]);

    let actual = parse_cluster_slots(input, &Str::from("bad-host")).expect("Failed to parse input");
    let expected = vec![
      SlotRange {
        start:                                 0,
        end:                                   5460,
        primary:                               Server {
          host:            "127.0.0.1".into(),
          port:            30001,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "09dbe9720cda62f7865eabc5fd8857c5d2678366".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "127.0.0.1".into(),
          port:            30004,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
      SlotRange {
        start:                                 5461,
        end:                                   10922,
        primary:                               Server {
          host:            "127.0.0.1".into(),
          port:            30002,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "c9d93d9f2c0c524ff34cc11838c2003d8c29e013".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "127.0.0.1".into(),
          port:            30005,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
      SlotRange {
        start:                                 10923,
        end:                                   16383,
        primary:                               Server {
          host:            "127.0.0.1".into(),
          port:            30003,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "044ec91f325b7595e76dbcb18cc688b6a5b434a1".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "127.0.0.1".into(),
          port:            30006,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
    ];
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_cluster_slots_example_null_hostname() {
    let first_slot_range = Value::Array(vec![
      0.into(),
      5460.into(),
      Value::Array(vec![
        Value::Null,
        30001.into(),
        "09dbe9720cda62f7865eabc5fd8857c5d2678366".into(),
        Value::Array(vec![]),
      ]),
      Value::Array(vec![
        Value::Null,
        30004.into(),
        "821d8ca00d7ccf931ed3ffc7e3db0599d2271abf".into(),
        Value::Array(vec![]),
      ]),
    ]);
    let second_slot_range = Value::Array(vec![
      5461.into(),
      10922.into(),
      Value::Array(vec![
        Value::Null,
        30002.into(),
        "c9d93d9f2c0c524ff34cc11838c2003d8c29e013".into(),
        Value::Array(vec![]),
      ]),
      Value::Array(vec![
        Value::Null,
        30005.into(),
        "faadb3eb99009de4ab72ad6b6ed87634c7ee410f".into(),
        Value::Array(vec![]),
      ]),
    ]);
    let third_slot_range = Value::Array(vec![
      10923.into(),
      16383.into(),
      Value::Array(vec![
        Value::Null,
        30003.into(),
        "044ec91f325b7595e76dbcb18cc688b6a5b434a1".into(),
        Value::Array(vec![]),
      ]),
      Value::Array(vec![
        Value::Null,
        30006.into(),
        "58e6e48d41228013e5d9c1c37c5060693925e97e".into(),
        Value::Array(vec![]),
      ]),
    ]);
    let input = Value::Array(vec![first_slot_range, second_slot_range, third_slot_range]);

    let actual = parse_cluster_slots(input, &Str::from("fake-host")).expect("Failed to parse input");
    let expected = vec![
      SlotRange {
        start:                                 0,
        end:                                   5460,
        primary:                               Server {
          host:            "fake-host".into(),
          port:            30001,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "09dbe9720cda62f7865eabc5fd8857c5d2678366".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "fake-host".into(),
          port:            30004,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
      SlotRange {
        start:                                 5461,
        end:                                   10922,
        primary:                               Server {
          host:            "fake-host".into(),
          port:            30002,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "c9d93d9f2c0c524ff34cc11838c2003d8c29e013".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "fake-host".into(),
          port:            30005,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
      SlotRange {
        start:                                 10923,
        end:                                   16383,
        primary:                               Server {
          host:            "fake-host".into(),
          port:            30003,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "044ec91f325b7595e76dbcb18cc688b6a5b434a1".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "fake-host".into(),
          port:            30006,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
    ];
    assert_eq!(actual, expected);
  }

  #[test]
  fn should_parse_cluster_slots_example_empty_hostname() {
    let first_slot_range = Value::Array(vec![
      0.into(),
      5460.into(),
      Value::Array(vec![
        Value::Null,
        30001.into(),
        "09dbe9720cda62f7865eabc5fd8857c5d2678366".into(),
        Value::Array(vec!["hostname".into(), "".into()]),
      ]),
      Value::Array(vec![
        Value::Null,
        30004.into(),
        "821d8ca00d7ccf931ed3ffc7e3db0599d2271abf".into(),
        Value::Array(vec!["hostname".into(), "".into()]),
      ]),
    ]);
    let second_slot_range = Value::Array(vec![
      5461.into(),
      10922.into(),
      Value::Array(vec![
        Value::Null,
        30002.into(),
        "c9d93d9f2c0c524ff34cc11838c2003d8c29e013".into(),
        Value::Array(vec!["hostname".into(), "".into()]),
      ]),
      Value::Array(vec![
        Value::Null,
        30005.into(),
        "faadb3eb99009de4ab72ad6b6ed87634c7ee410f".into(),
        Value::Array(vec!["hostname".into(), "".into()]),
      ]),
    ]);
    let third_slot_range = Value::Array(vec![
      10923.into(),
      16383.into(),
      Value::Array(vec![
        Value::Null,
        30003.into(),
        "044ec91f325b7595e76dbcb18cc688b6a5b434a1".into(),
        Value::Array(vec!["hostname".into(), "".into()]),
      ]),
      Value::Array(vec![
        Value::Null,
        30006.into(),
        "58e6e48d41228013e5d9c1c37c5060693925e97e".into(),
        Value::Array(vec!["hostname".into(), "".into()]),
      ]),
    ]);
    let input = Value::Array(vec![first_slot_range, second_slot_range, third_slot_range]);

    let actual = parse_cluster_slots(input, &Str::from("fake-host")).expect("Failed to parse input");
    let expected = vec![
      SlotRange {
        start:                                 0,
        end:                                   5460,
        primary:                               Server {
          host:            "fake-host".into(),
          port:            30001,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "09dbe9720cda62f7865eabc5fd8857c5d2678366".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "fake-host".into(),
          port:            30004,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
      SlotRange {
        start:                                 5461,
        end:                                   10922,
        primary:                               Server {
          host:            "fake-host".into(),
          port:            30002,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "c9d93d9f2c0c524ff34cc11838c2003d8c29e013".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "fake-host".into(),
          port:            30005,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
      SlotRange {
        start:                                 10923,
        end:                                   16383,
        primary:                               Server {
          host:            "fake-host".into(),
          port:            30003,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        },
        id:                                    "044ec91f325b7595e76dbcb18cc688b6a5b434a1".into(),
        #[cfg(feature = "replicas")]
        replicas:                              vec![Server {
          host:            "fake-host".into(),
          port:            30006,
          #[cfg(any(
            feature = "enable-native-tls",
            feature = "enable-rustls",
            feature = "enable-rustls-ring"
          ))]
          tls_server_name: None,
        }],
      },
    ];
    assert_eq!(actual, expected);
  }
}
