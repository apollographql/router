use std::time::Duration;
use std::time::Instant;

use fred::prelude::*;
use fred::socket2::TcpKeepalive;
use redis_experiments::AsciiWhitespaceSeparated;
use redis_experiments::Cache;
use redis_experiments::CacheConfig;
use redis_experiments::Expire;

const TEMPORARY: bool = true;

async fn cache() -> Cache {
    let config = std::env::var("REDIS_URL")
        .map(|url| Config::from_url(&url).unwrap())
        .unwrap_or_default();
    let client = Builder::from_config(config)
        .with_connection_config(|config| {
            config.tcp = TcpConfig {
                nodelay: Some(true),
                keepalive: Some(TcpKeepalive::new().with_time(Duration::from_secs(600))),
                ..Default::default()
            }
        })
        .build()
        .unwrap();
    // let client = Client::default();

    client.init().await.unwrap();
    Cache {
        client,
        config: CacheConfig {
            namespace: Some(CacheConfig::random_namespace()),
            temporary_seconds: TEMPORARY.then_some(600),
            index_name: "invalidation".into(),
            indexed_document_id_prefix: "".into(),
            invalidation_keys_field_name: "invalidation_keys".into(),
        },
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    test_tag_escaping();
    test_create_existing_index().await;
    test_few().await;
    // test_weird_keys().await;
    test_many().await;
    println!("Done!")
}

fn test_tag_escaping() {
    assert_eq!(
        redis_experiments::escape_redisearch_tag_filter(
            r##"!"#$%&'()*+,-./09:;<=>?@AZ[\]^_`az{|}~"##
        ),
        r##"\!\"\#\$\%\&\'\(\)\*\+\,\-\.\/09\:\;\<\=\>\?\@AZ\[\\\]\^_\`az\{\|\}\~"##
    )
}

async fn test_create_existing_index() {
    let cache = cache().await;
    cache.create_index().await.unwrap();
    cache.create_index_if_not_exists().await.unwrap();
    cache.drop_index(true).await.unwrap();
}

async fn test_few() {
    let cache = cache().await;
    cache.create_index().await.unwrap();
    let expire = Expire::In { seconds: 600 };
    cache
        .insert_hash_document(
            "docA",
            expire,
            AsciiWhitespaceSeparated("key1 key2"),
            [("data", "A")],
        )
        .await
        .unwrap();
    cache
        .insert_hash_document(
            "docB",
            expire,
            AsciiWhitespaceSeparated("key1"),
            [("data", "B")],
        )
        .await
        .unwrap();
    cache
        .insert_hash_document(
            "docC",
            expire,
            AsciiWhitespaceSeparated("key2"),
            [("data", "C")],
        )
        .await
        .unwrap();
    let get = async |id: &str| {
        cache
            .get_hash_document::<Option<String>>(id, "data")
            .await
            .unwrap()
    };
    assert_eq!(get("docA").await.as_deref(), Some("A"));
    assert_eq!(get("docB").await.as_deref(), Some("B"));
    assert_eq!(get("docC").await.as_deref(), Some("C"));
    assert_eq!(
        cache
            .invalidate(AsciiWhitespaceSeparated("key1 unused"))
            .await
            .unwrap(),
        2
    );
    assert_eq!(get("docA").await.as_deref(), None);
    assert_eq!(get("docB").await.as_deref(), None);
    assert_eq!(get("docC").await.as_deref(), Some("C"));
    cache.drop_index(true).await.unwrap();
}

async fn test_weird_keys() {
    let cache = cache().await;
    cache.create_index().await.unwrap();
    let expire = Expire::In { seconds: 600 };
    // Non-ASCII, all ASCII punctiation, leading digit
    let key1 = AsciiWhitespaceSeparated(r##"k1|🔑"##);
    let key2 = AsciiWhitespaceSeparated(r##"k2,!"#$%&'()*+,-./:;<=>?@[\]^_`{|}~"##);
    let key3 = AsciiWhitespaceSeparated(r##"0k"##);
    let not_key1 = AsciiWhitespaceSeparated("k1");
    let not_key2 = AsciiWhitespaceSeparated("k2");
    let not_key3 = AsciiWhitespaceSeparated("k");
    cache
        .insert_hash_document("x", expire, key1, [("data", "X")])
        .await
        .unwrap();
    cache
        .insert_hash_document("y", expire, key2, [("data", "Y")])
        .await
        .unwrap();
    cache
        .insert_hash_document("z", expire, key3, [("data", "Z")])
        .await
        .unwrap();
    assert_eq!(cache.invalidate(not_key1).await.unwrap(), 0);
    assert_eq!(cache.invalidate(not_key2).await.unwrap(), 0);
    assert_eq!(cache.invalidate(not_key3).await.unwrap(), 0);
    assert_eq!(cache.invalidate(key1).await.unwrap(), 1);
    assert_eq!(cache.invalidate(key2).await.unwrap(), 1);
    assert_eq!(cache.invalidate(key3).await.unwrap(), 1);
    cache.drop_index(true).await.unwrap();
}

async fn test_many() {
    let cache = cache().await;
    cache.create_index().await.unwrap();
    let expire = Expire::In { seconds: 600 };
    let start = Instant::now();
    for count in [100, 1_000, 10_000] {
        println!("{count} entries…");
        for i in 0..count {
            cache
                .insert_hash_document(
                    &format!("doc{i}"),
                    expire,
                    AsciiWhitespaceSeparated("key1 key2"),
                    [("data", "A")],
                )
                .await
                .unwrap();
        }
        let duration = start.elapsed();
        println!("… inserted (one by one) in {} ms", duration.as_millis());

        let start = Instant::now();
        let deleted = cache
            .invalidate(AsciiWhitespaceSeparated("key2"))
            .await
            .unwrap();
        let duration = start.elapsed();
        println!("… invalidated (in batch) in {} ms", duration.as_millis());
        println!();
        assert_eq!(deleted, count)
    }
    // cache.drop_index(true).await.unwrap();
}
