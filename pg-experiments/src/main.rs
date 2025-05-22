use std::time::Instant;

use humansize::DECIMAL;
use humansize::FormatSize;
use pg_experiments::BatchDocument;
use pg_experiments::Cache;
use pg_experiments::CacheConfig;
use pg_experiments::Expire;
use sqlx::postgres::PgPoolOptions;

const TEMPORARY: bool = true;

async fn cache() -> Cache {
    let pool = PgPoolOptions::new()
        .max_connections(10)
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
    let cache = cache().await;
    cache.migrate().await.unwrap();
    test_few().await;
    test_many().await;
    test_real_world().await;
    test_many_average_payload().await;
    test_many_big_payload().await;
    println!("Done!");
}

async fn test_few() {
    println!("===== test_few =====");
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
    println!("===== test_many =====");
    let cache = cache().await;
    let expire = Expire::In { seconds: 600 };
    let start = Instant::now();
    let data = String::from("a");
    let size = data.len();

    for count in [100u64, 500, 1_000, 10_000] {
        cache.truncate().await.unwrap();
        println!("{count} entries…");
        println!(
            "Potential response body size: {}",
            (size as u64 * count).format_size(DECIMAL)
        );
        for i in 0..count {
            cache
                .insert_hash_document(
                    &format!("doc{i}"),
                    expire,
                    vec!["key1".to_string(), "key2".to_string()],
                    serde_json::json!({"data": data}),
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
}

// Max response size is around 280kB
// Max entity type is around 50

async fn test_real_world() {
    println!("===== test_real_world =====");
    let cache = cache().await;
    let expire = Expire::In { seconds: 600 };
    let data = (0..6_000).map(|_| "x").collect::<Vec<&str>>().join("");
    let entity_size = data.len();

    for count in [10, 20, 30, 40, 50, 100] {
        cache.truncate().await.unwrap();
        println!("{count} entries…");
        println!("Entity size: {}", (entity_size as u64).format_size(DECIMAL));
        println!(
            "Potential response body size: {}",
            (entity_size as u64 * count).format_size(DECIMAL)
        );
        let start = Instant::now();

        let batch_docs = (0..count)
            .map(|idx| BatchDocument {
                cache_key: format!("doc{idx}"),
                data: data.clone(),
                invalidation_keys: vec!["key1".to_string(), "key2".to_string()],
                expire,
            })
            .collect();
        cache
            .insert_hash_document_in_batch(batch_docs)
            .await
            .unwrap();

        let duration = start.elapsed();
        println!("… inserted (100 by 100) in {} ms", duration.as_millis());

        let start = Instant::now();
        let deleted = cache.invalidate(vec!["key2".to_string()]).await.unwrap();
        let duration = start.elapsed();
        println!("… invalidated (in batch) in {} ms", duration.as_millis());
        println!();
        assert_eq!(deleted, count)
    }
}

async fn test_many_average_payload() {
    println!("===== test_many_average_payload =====");
    let cache = cache().await;
    let expire = Expire::In { seconds: 600 };
    let data = (0..50_000).map(|_| "x").collect::<Vec<&str>>().join("");
    let size = data.len();

    for count in [100, 1_000, 10_000] {
        cache.truncate().await.unwrap();
        println!("{count} entries…");
        println!(
            "Potential response body size: {}",
            (size as u64 * count).format_size(DECIMAL)
        );
        let start = Instant::now();

        let batch_docs = (0..count)
            .map(|idx| BatchDocument {
                cache_key: format!("doc{idx}"),
                data: data.clone(),
                invalidation_keys: vec!["key1".to_string(), "key2".to_string()],
                expire,
            })
            .collect();
        cache
            .insert_hash_document_in_batch(batch_docs)
            .await
            .unwrap();

        let duration = start.elapsed();
        println!("… inserted (100 by 100) in {} ms", duration.as_millis());

        let start = Instant::now();
        let deleted = cache.invalidate(vec!["key2".to_string()]).await.unwrap();
        let duration = start.elapsed();
        println!("… invalidated (in batch) in {} ms", duration.as_millis());
        println!();
        assert_eq!(deleted, count)
    }
}

async fn test_many_big_payload() {
    println!("===== test_many_big_payload =====");
    let cache = cache().await;
    let expire = Expire::In { seconds: 600 };
    let data = (0..1_000_000).map(|_| "x").collect::<Vec<&str>>().join("");
    let size = data.len();

    for count in [100, 1_000, 10_000] {
        cache.truncate().await.unwrap();
        println!("{count} entries…");
        println!(
            "Potential response body size: {}",
            (size as u64 * count).format_size(DECIMAL)
        );
        let start = Instant::now();

        let mut batch_docs = Vec::with_capacity(100);
        for idx in 0..count {
            batch_docs.push(BatchDocument {
                cache_key: format!("doc{idx}"),
                data: data.clone(),
                invalidation_keys: vec!["key1".to_string(), "key2".to_string()],
                expire,
            });
            if idx % 100 == 0 {
                cache
                    .insert_hash_document_in_batch(std::mem::replace(
                        &mut batch_docs,
                        Vec::with_capacity(100),
                    ))
                    .await
                    .unwrap();
            }
        }
        cache
            .insert_hash_document_in_batch(batch_docs)
            .await
            .unwrap();

        let duration = start.elapsed();
        println!("… inserted (100 by 100) in {} ms", duration.as_millis());

        let start = Instant::now();
        let deleted = cache.invalidate(vec!["key2".to_string()]).await.unwrap();
        let duration = start.elapsed();
        println!("… invalidated (in batch) in {} ms", duration.as_millis());
        println!();
        assert_eq!(deleted, count)
    }
}
