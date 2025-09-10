use rand::{self, distributions::Alphanumeric, Rng};
use std::{
  env,
  error::Error,
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
  },
};

pub fn incr_atomic(size: &Arc<AtomicUsize>) -> usize {
  size.fetch_add(1, Ordering::AcqRel).saturating_add(1)
}

pub fn incr_by_atomic(size: &Arc<AtomicUsize>, n: usize) -> usize {
  size.fetch_add(n, Ordering::AcqRel).saturating_add(n)
}

pub fn read_atomic(size: &Arc<AtomicUsize>) -> usize {
  size.load(Ordering::Acquire)
}

pub fn set_atomic(size: &Arc<AtomicUsize>, val: usize) -> usize {
  size.swap(val, Ordering::SeqCst)
}

pub fn read_auth_env() -> (Option<String>, Option<String>) {
  let username = env::var_os("REDIS_USERNAME").and_then(|s| s.into_string().ok());
  let password = env::var_os("REDIS_PASSWORD").and_then(|s| s.into_string().ok());

  (username, password)
}

pub fn random_string(len: usize) -> String {
  rand::thread_rng()
    .sample_iter(&Alphanumeric)
    .take(len)
    .map(char::from)
    .collect()
}

pub fn crash(error: impl Error) {
  println!("{:?}", error);
  std::process::exit(1);
}
