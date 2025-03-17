use std::collections::HashMap;
use std::sync::Arc;

use bytes::Bytes;
use fred::error::RedisErrorKind;
use fred::mocks::MockCommand;
use fred::mocks::Mocks;
use fred::prelude::RedisError;
use fred::prelude::RedisValue;
use parking_lot::Mutex;

/// `pub` for tests
#[derive(Debug, Default)]
pub struct MockStore {
    map: Arc<Mutex<HashMap<Bytes, Bytes>>>,
}

impl Mocks for MockStore {
    fn process_command(&self, command: MockCommand) -> Result<RedisValue, RedisError> {
        println!("mock received redis command: {command:?}");

        match &*command.cmd {
            "GET" => {
                if let Some(RedisValue::Bytes(b)) = command.args.first() {
                    if let Some(bytes) = self.map.lock().get(b) {
                        println!("-> returning {:?}", std::str::from_utf8(bytes));
                        return Ok(RedisValue::Bytes(bytes.clone()));
                    }
                }
            }
            "MGET" => {
                let mut result: Vec<RedisValue> = Vec::new();

                let mut args_it = command.args.iter();
                while let Some(RedisValue::Bytes(key)) = args_it.next() {
                    if let Some(bytes) = self.map.lock().get(key) {
                        result.push(RedisValue::Bytes(bytes.clone()));
                    } else {
                        result.push(RedisValue::Null);
                    }
                }
                return Ok(RedisValue::Array(result));
            }
            "SET" => {
                if let (Some(RedisValue::Bytes(key)), Some(RedisValue::Bytes(value))) =
                    (command.args.first(), command.args.get(1))
                {
                    self.map.lock().insert(key.clone(), value.clone());
                    return Ok(RedisValue::Null);
                }
            }
            "MSET" => {
                let mut args_it = command.args.iter();
                while let (Some(RedisValue::Bytes(key)), Some(RedisValue::Bytes(value))) =
                    (args_it.next(), args_it.next())
                {
                    self.map.lock().insert(key.clone(), value.clone());
                }
                return Ok(RedisValue::Null);
            }
            //FIXME: this is not working because fred's mock never sends the response to SCAN to the client
            /*"SCAN" => {
                let mut args_it = command.args.iter();
                if let (
                    Some(RedisValue::String(cursor)),
                    Some(RedisValue::String(_match)),
                    Some(RedisValue::String(pattern)),
                    Some(RedisValue::String(_count)),
                    Some(RedisValue::Integer(max_count)),
                ) = (
                    args_it.next(),
                    args_it.next(),
                    args_it.next(),
                    args_it.next(),
                    args_it.next(),
                ) {
                    let cursor: usize = cursor.parse().unwrap();

                    if cursor > self.map.lock().len() {
                        let res = RedisValue::Array(vec![
                            RedisValue::String(0.to_string().into()),
                            RedisValue::Array(Vec::new()),
                        ]);
                        println!("result: {res:?}");

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
                            println!("seen key at index {i}");
                            count = i + 1;
                            key
                        })
                        .filter(|key| regex.is_match(&*key))
                        .map(|key| RedisValue::Bytes(key.clone()))
                        .take(*max_count as usize)
                        .collect();

                    println!("scan returns cursor {count}, for {} values", res.len());
                    let res = RedisValue::Array(vec![
                        RedisValue::String(count.to_string().into()),
                        RedisValue::Array(res),
                    ]);
                    println!("result: {res:?}");

                    return Ok(res);
                } else {
                    panic!()
                }
            }*/
            _ => {
                panic!("unrecoginzed command: {command:?}")
            }
        }
        Err(RedisError::new(RedisErrorKind::NotFound, "mock not found"))
    }
}
