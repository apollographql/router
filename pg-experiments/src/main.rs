use std::time::Duration;
use std::time::Instant;

use pg_experiments::Cache;
use pg_experiments::CacheConfig;
use pg_experiments::Expire;
use sqlx::postgres::PgPoolOptions;

const TEMPORARY: bool = true;

async fn cache() -> Cache {
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&std::env::var("DATABASE_URL").expect("DATABASE_URL env var is not set"))
        .await
        .unwrap();

    Cache {
        client: pool,
        config: CacheConfig {
            namespace: None,
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
    cache().await.migrate().await.unwrap();
    test_few().await;
    // test_weird_keys().await;
    test_many().await;
    println!("Done!");
}

async fn test_few() {
    let cache = cache().await;
    cache.truncate().await.unwrap();
    let expire = Expire::In { seconds: 600 };
    cache
        .insert_hash_document(
            "docA",
            expire,
            vec!["key1".to_string(), "key2".to_string()],
            serde_json::json!({"data": "A"}),
        )
        .await
        .unwrap();
    cache
        .insert_hash_document(
            "docB",
            expire,
            vec!["key1".to_string()],
            serde_json::json!({"data": "B"}),
        )
        .await
        .unwrap();
    cache
        .insert_hash_document(
            "docC",
            expire,
            vec!["key2".to_string()],
            serde_json::json!({"data": "C"}),
        )
        .await
        .unwrap();
    let get = async |id: &str| cache.get_hash_document(id).await.map(|r| r.data);
    assert_eq!(get("docA").await.unwrap(), serde_json::json!({"data": "A"}));
    assert_eq!(get("docB").await.unwrap(), serde_json::json!({"data": "B"}));
    assert_eq!(get("docC").await.unwrap(), serde_json::json!({"data": "C"}));
    assert_eq!(
        cache
            .invalidate(vec!["key1".to_string(), "unused".to_string()])
            .await
            .unwrap(),
        2
    );
    assert!(get("docA").await.is_err());
    assert!(get("docB").await.is_err());
    assert_eq!(get("docC").await.unwrap(), serde_json::json!({"data": "C"}));
}

async fn test_many() {
    let cache = cache().await;
    let expire = Expire::In { seconds: 600 };
    let start = Instant::now();
    for count in [100, 1_000, 10_000] {
        println!("truncate");
        cache.truncate().await.unwrap();
        println!("{count} entries…");
        for i in 0..count {
            cache
                .insert_hash_document(
                    &format!("doc{i}"),
                    expire,
                    vec!["key1".to_string(), "key2".to_string()],
                    serde_json::json!({"data": "A"}),
                )
                .await
                .unwrap();
        }
        let duration = start.elapsed();
        println!("… inserted (one by one) in {} ms", duration.as_millis());

        let start = Instant::now();
        let deleted = cache.invalidate(vec!["key2".to_string()]).await.unwrap();
        let duration = start.elapsed();
        println!("… invalidated (in batch) in {} ms", duration.as_millis());
        println!();
        assert_eq!(deleted, count)
    }
    // cache.drop_index(true).await.unwrap();
}
