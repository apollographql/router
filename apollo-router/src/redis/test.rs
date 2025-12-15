use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use fred::mocks::MockCommand;
use fred::mocks::Mocks;
use parking_lot::Mutex;
use regex::Regex;

// Mock the Redis connection to be able to simulate a timeout error coming from within
// the `fred` client
#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
#[derive(Default, Debug, Clone)]
pub(crate) struct MockStorageTimeout(Arc<parking_lot::RwLock<Vec<MockCommand>>>);

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
impl Mocks for MockStorageTimeout {
    fn process_command(
        &self,
        command: MockCommand,
    ) -> Result<fred::types::Value, fred::error::Error> {
        self.0.write().push(command);

        let timeout_error = fred::error::Error::new(fred::error::ErrorKind::Timeout, "");
        Err(timeout_error)
    }
}

#[cfg(all(
    test,
    any(not(feature = "ci"), all(target_arch = "x86_64", target_os = "linux"))
))]
impl MockStorageTimeout {
    pub(crate) fn commands(&self) -> Vec<MockCommand> {
        self.0.read().clone()
    }
}

#[derive(Debug)]
pub(crate) struct MockStorage {
    map: Arc<Mutex<HashMap<Bytes, Bytes>>>,
}

impl MockStorage {
    pub(crate) fn new() -> MockStorage {
        MockStorage {
            map: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl Mocks for MockStorage {
    fn process_command(
        &self,
        command: MockCommand,
    ) -> Result<fred::types::Value, fred::error::Error> {
        eprintln!("mock received redis command: {command:?}");

        match &*command.cmd {
            "GET" => {
                return if let Some(fred::types::Value::Bytes(b)) = command.args.first()
                    && let Some(bytes) = self.map.lock().get(b)
                {
                    Ok(fred::types::Value::Bytes(bytes.clone()))
                } else {
                    Ok(fred::types::Value::Null)
                };
            }
            "MGET" => {
                let mut result: Vec<fred::types::Value> = Vec::new();

                let mut args_it = command.args.iter();
                while let Some(fred::types::Value::Bytes(key)) = args_it.next() {
                    if let Some(bytes) = self.map.lock().get(key) {
                        result.push(fred::types::Value::Bytes(bytes.clone()));
                    } else {
                        result.push(fred::types::Value::Null);
                    }
                }
                return Ok(fred::types::Value::Array(result));
            }
            "SET" => {
                if let (
                    Some(fred::types::Value::Bytes(key)),
                    Some(fred::types::Value::Bytes(value)),
                ) = (command.args.first(), command.args.get(1))
                {
                    self.map.lock().insert(key.clone(), value.clone());
                    return Ok(fred::types::Value::Null);
                }
            }
            "MSET" => {
                let mut args_it = command.args.iter();
                while let (
                    Some(fred::types::Value::Bytes(key)),
                    Some(fred::types::Value::Bytes(value)),
                ) = (args_it.next(), args_it.next())
                {
                    self.map.lock().insert(key.clone(), value.clone());
                }
                return Ok(fred::types::Value::Null);
            }
            //FIXME: this is not working because fred's mock never sends the response to SCAN to the client
            "SCAN" => {
                let mut args_it = command.args.iter();
                if let (
                    Some(fred::types::Value::String(cursor)),
                    Some(fred::types::Value::String(_match)),
                    Some(fred::types::Value::String(pattern)),
                    Some(fred::types::Value::String(_count)),
                    Some(fred::types::Value::Integer(max_count)),
                ) = (
                    args_it.next(),
                    args_it.next(),
                    args_it.next(),
                    args_it.next(),
                    args_it.next(),
                ) {
                    let cursor: usize = cursor.parse()?;

                    if cursor > self.map.lock().len() {
                        let res = fred::types::Value::Array(vec![
                            fred::types::Value::String(0.to_string().into()),
                            fred::types::Value::Array(Vec::new()),
                        ]);
                        return Ok(res);
                    }

                    let regex = Regex::new(pattern).unwrap();
                    let mut count = 0;
                    let res: Vec<_> = self
                        .map
                        .lock()
                        .keys()
                        .enumerate()
                        .skip(cursor)
                        .map(|(i, key)| {
                            count = i + 1;
                            key
                        })
                        .filter(|key| regex.is_match(std::str::from_utf8(key).unwrap()))
                        .map(|key| fred::types::Value::Bytes(key.clone()))
                        .take(*max_count as usize)
                        .collect();

                    let res = fred::types::Value::Array(vec![
                        fred::types::Value::String(count.to_string().into()),
                        fred::types::Value::Array(res),
                    ]);
                    return Ok(res);
                } else {
                    panic!()
                }
            }
            "DEL" => {
                let mut count = 0;
                let mut args_it = command.args.iter();
                while let Some(fred::types::Value::Bytes(key)) = args_it.next() {
                    if self.map.lock().remove(key).is_some() {
                        count += 1
                    }
                }

                return Ok(fred::types::Value::Integer(count));
            }
            _ => {
                panic!("unrecognized command: {command:?}")
            }
        }
        Err(fred::error::Error::new(
            fred::error::ErrorKind::NotFound,
            "mock not found",
        ))
    }
}
