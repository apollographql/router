use super::utils::should_use_sentinel_config;
use fred::{interfaces::PubsubInterface, prelude::*};
use futures::{Stream, StreamExt};
use std::{collections::HashMap, time::Duration};
use tokio::time::sleep;

const CHANNEL1: &str = "foo";
const CHANNEL2: &str = "bar";
const CHANNEL3: &str = "baz";
const FAKE_MESSAGE: &str = "wibble";
const NUM_MESSAGES: i64 = 20;

async fn wait_a_sec() {
  // pubsub command responses arrive out of band therefore it's hard to synchronize calls across clients. in CI the
  // machines are pretty small and subscribe-then-check commands sometimes give strange results due to these timing
  // issues.
  tokio::time::sleep(Duration::from_millis(20)).await;
}

pub async fn should_publish_and_recv_messages(client: Client, _: Config) -> Result<(), Error> {
  let subscriber_client = client.clone_new();
  subscriber_client.connect();
  subscriber_client.wait_for_connect().await?;
  subscriber_client.subscribe(CHANNEL1).await?;

  let subscriber_jh = tokio::spawn(async move {
    let mut message_stream = subscriber_client.message_rx();

    let mut count = 0;
    while count < NUM_MESSAGES {
      if let Ok(message) = message_stream.recv().await {
        assert_eq!(CHANNEL1, message.channel);
        assert_eq!(format!("{}-{}", FAKE_MESSAGE, count), message.value.as_str().unwrap());
        count += 1;
      }
    }

    Ok::<_, Error>(())
  });

  sleep(Duration::from_secs(1)).await;
  for idx in 0 .. NUM_MESSAGES {
    // https://redis.io/commands/publish#return-value
    let _: () = client.publish(CHANNEL1, format!("{}-{}", FAKE_MESSAGE, idx)).await?;

    // pubsub messages may arrive out of order due to cross-cluster broadcasting
    sleep(Duration::from_millis(50)).await;
  }
  let _ = subscriber_jh.await?;

  Ok(())
}

pub async fn should_ssubscribe_and_recv_messages(client: Client, _: Config) -> Result<(), Error> {
  let subscriber_client = client.clone_new();
  subscriber_client.connect();
  subscriber_client.wait_for_connect().await?;
  subscriber_client.ssubscribe(CHANNEL1).await?;

  let subscriber_jh = tokio::spawn(async move {
    let mut message_stream = subscriber_client.message_rx();

    let mut count = 0;
    while count < NUM_MESSAGES {
      if let Ok(message) = message_stream.recv().await {
        assert_eq!(CHANNEL1, message.channel);
        assert_eq!(format!("{}-{}", FAKE_MESSAGE, count), message.value.as_str().unwrap());
        count += 1;
      }
    }

    Ok::<_, Error>(())
  });

  sleep(Duration::from_secs(1)).await;
  for idx in 0 .. NUM_MESSAGES {
    // https://redis.io/commands/publish#return-value
    let _: () = client.spublish(CHANNEL1, format!("{}-{}", FAKE_MESSAGE, idx)).await?;

    // pubsub messages may arrive out of order due to cross-cluster broadcasting
    sleep(Duration::from_millis(50)).await;
  }
  let _ = subscriber_jh.await?;

  Ok(())
}

pub async fn should_psubscribe_and_recv_messages(client: Client, _: Config) -> Result<(), Error> {
  let channels = vec![CHANNEL1, CHANNEL2, CHANNEL3];
  let subscriber_channels = channels.clone();

  let subscriber_client = client.clone_new();
  subscriber_client.connect();
  subscriber_client.wait_for_connect().await?;
  subscriber_client.psubscribe(channels.clone()).await?;

  let subscriber_jh = tokio::spawn(async move {
    let mut message_stream = subscriber_client.message_rx();

    let mut count = 0;
    while count < NUM_MESSAGES {
      if let Ok(message) = message_stream.recv().await {
        assert!(subscriber_channels.contains(&&*message.channel));
        assert_eq!(format!("{}-{}", FAKE_MESSAGE, count), message.value.as_str().unwrap());
        count += 1;
      }
    }

    Ok::<_, Error>(())
  });

  sleep(Duration::from_secs(1)).await;
  for idx in 0 .. NUM_MESSAGES {
    let channel = channels[idx as usize % channels.len()];

    // https://redis.io/commands/publish#return-value
    let _: () = client.publish(channel, format!("{}-{}", FAKE_MESSAGE, idx)).await?;

    // pubsub messages may arrive out of order due to cross-cluster broadcasting
    sleep(Duration::from_millis(50)).await;
  }
  let _ = subscriber_jh.await?;

  Ok(())
}

pub async fn should_unsubscribe_from_all(publisher: Client, _: Config) -> Result<(), Error> {
  let subscriber = publisher.clone_new();
  let connection = subscriber.connect();
  subscriber.wait_for_connect().await?;
  subscriber.subscribe(vec![CHANNEL1, CHANNEL2, CHANNEL3]).await?;
  let mut message_stream = subscriber.message_rx();

  tokio::spawn(async move {
    if let Ok(message) = message_stream.recv().await {
      // unsubscribe without args will result in 3 messages in this case, and none should show up here
      panic!("Recv unexpected pubsub message: {:?}", message);
    }

    Ok::<_, Error>(())
  });

  subscriber.unsubscribe(()).await?;
  sleep(Duration::from_secs(1)).await;

  // make sure the response buffer is flushed correctly by this point
  assert_eq!(subscriber.ping::<String>(None).await?, "PONG");
  assert_eq!(subscriber.ping::<String>(None).await?, "PONG");
  assert_eq!(subscriber.ping::<String>(None).await?, "PONG");

  subscriber.quit().await?;
  let _ = connection.await?;
  Ok(())
}

pub async fn should_get_pubsub_channels(client: Client, _: Config) -> Result<(), Error> {
  let subscriber = client.clone_new();
  subscriber.connect();
  subscriber.wait_for_connect().await?;

  let channels: Vec<String> = client.pubsub_channels("*").await?;
  let expected_len = if should_use_sentinel_config() {
    // "__sentinel__:hello" is always there
    1
  } else {
    0
  };
  assert_eq!(channels.len(), expected_len);

  subscriber.subscribe("foo").await?;
  subscriber.subscribe("bar").await?;
  wait_a_sec().await;
  let mut channels: Vec<String> = client.pubsub_channels("*").await?;
  channels.sort();

  let expected = if should_use_sentinel_config() {
    vec!["__sentinel__:hello".into(), "bar".to_string(), "foo".to_string()]
  } else {
    vec!["bar".to_string(), "foo".to_string()]
  };
  assert_eq!(channels, expected);
  Ok(())
}

pub async fn should_get_pubsub_numpat(client: Client, _: Config) -> Result<(), Error> {
  let subscriber = client.clone_new();
  subscriber.connect();
  subscriber.wait_for_connect().await?;

  assert_eq!(client.pubsub_numpat::<i64>().await?, 0);
  subscriber.psubscribe("foo*").await?;
  subscriber.psubscribe("bar*").await?;
  wait_a_sec().await;
  assert_eq!(client.pubsub_numpat::<i64>().await?, 2);

  Ok(())
}

pub async fn should_get_pubsub_nunmsub(client: Client, _: Config) -> Result<(), Error> {
  let subscriber = client.clone_new();
  subscriber.connect();
  subscriber.wait_for_connect().await?;

  let mut expected: HashMap<String, i64> = HashMap::new();
  expected.insert("foo".into(), 0);
  expected.insert("bar".into(), 0);
  let channels: HashMap<String, i64> = client.pubsub_numsub(vec!["foo", "bar"]).await?;
  assert_eq!(channels, expected);

  subscriber.subscribe("foo").await?;
  subscriber.subscribe("bar").await?;
  wait_a_sec().await;
  let channels: HashMap<String, i64> = client.pubsub_numsub(vec!["foo", "bar"]).await?;

  let mut expected: HashMap<String, i64> = HashMap::new();
  expected.insert("foo".into(), 1);
  expected.insert("bar".into(), 1);
  assert_eq!(channels, expected);

  Ok(())
}

pub async fn should_get_pubsub_shard_channels(client: Client, _: Config) -> Result<(), Error> {
  let subscriber = client.clone_new();
  subscriber.connect();
  subscriber.wait_for_connect().await?;

  let channels: Vec<String> = client.pubsub_shardchannels("{1}*").await?;
  assert!(channels.is_empty());

  subscriber.ssubscribe("{1}foo").await?;
  subscriber.ssubscribe("{1}bar").await?;
  wait_a_sec().await;

  let mut channels: Vec<String> = client.pubsub_shardchannels("{1}*").await?;
  channels.sort();
  assert_eq!(channels, vec!["{1}bar".to_string(), "{1}foo".to_string()]);

  Ok(())
}

pub async fn should_get_pubsub_shard_numsub(client: Client, _: Config) -> Result<(), Error> {
  let subscriber = client.clone_new();
  subscriber.connect();
  subscriber.wait_for_connect().await?;

  let mut expected: HashMap<String, i64> = HashMap::new();
  expected.insert("foo{1}".into(), 0);
  expected.insert("bar{1}".into(), 0);
  let channels: HashMap<String, i64> = client.pubsub_shardnumsub(vec!["foo{1}", "bar{1}"]).await?;
  assert_eq!(channels, expected);

  subscriber.ssubscribe("foo{1}").await?;
  subscriber.ssubscribe("bar{1}").await?;
  wait_a_sec().await;
  let channels: HashMap<String, i64> = client.pubsub_shardnumsub(vec!["foo{1}", "bar{1}"]).await?;

  let mut expected: HashMap<String, i64> = HashMap::new();
  expected.insert("foo{1}".into(), 1);
  expected.insert("bar{1}".into(), 1);
  assert_eq!(channels, expected);

  Ok(())
}
