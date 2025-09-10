use super::utils::{read_env_var, should_use_sentinel_config};
use fred::{
  clients::Client,
  error::Error,
  interfaces::*,
  types::{config::Config, Value},
};
use std::collections::HashMap;

// the docker image we use for sentinel tests doesn't allow for configuring users, just passwords,
// so for the tests here we just use an empty username so it uses the `default` user
fn read_redis_username() -> Option<String> {
  if should_use_sentinel_config() {
    None
  } else {
    read_env_var("REDIS_USERNAME")
  }
}

fn check_env_creds() -> (Option<String>, Option<String>) {
  (read_redis_username(), read_env_var("REDIS_PASSWORD"))
}

// note: currently this only works in CI against the centralized server
pub async fn should_auth_as_test_user(client: Client, _: Config) -> Result<(), Error> {
  let (username, password) = check_env_creds();
  if let Some(password) = password {
    let _: () = client.auth(username, password).await?;
    let _: () = client.ping(None).await?;
  }

  Ok(())
}

// FIXME currently this only works in CI against the centralized server
pub async fn should_auth_as_test_user_via_config(_: Client, mut config: Config) -> Result<(), Error> {
  let (username, password) = check_env_creds();
  if let Some(password) = password {
    config.username = username;
    config.password = Some(password);
    let client = Client::new(config, None, None, None);
    client.connect();
    client.wait_for_connect().await?;
    let _: () = client.ping(None).await?;
  }

  Ok(())
}

pub async fn should_run_acl_getuser(client: Client, _: Config) -> Result<(), Error> {
  let user: HashMap<String, Value> = client.acl_getuser("default").await?;
  let flags: Vec<String> = user.get("flags").unwrap().clone().convert()?;
  assert!(flags.contains(&"on".to_string()));

  Ok(())
}
