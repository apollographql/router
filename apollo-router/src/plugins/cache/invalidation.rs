use futures::StreamExt;
use tower::BoxError;

use crate::{cache::redis::RedisCacheStorage, notification::HandleStream, Notify};

pub(crate) struct Invalidation {
    storage: RedisCacheStorage,
    notify: Notify<InvalidationTopic, Vec<InvalidationRequest>>,
}

#[derive(Copy, Clone, Hash, PartialEq, Eq)]
pub(crate) struct InvalidationTopic;

#[derive(Clone, Debug)]
pub(crate) struct InvalidationRequest {}

impl Invalidation {
    pub(crate) async fn new(storage: RedisCacheStorage) -> Result<Self, BoxError> {
        let mut notify = Notify::new(None, None, None);
        let (handle, _b) = notify.create_or_subscribe(InvalidationTopic, false).await?;
        let s = storage.clone();
        tokio::task::spawn(async move { start(s, handle.into_stream()).await });
        Ok(Self { storage, notify })
    }
}

impl InvalidationRequest {
    fn key_prefix(&self) -> String {
        todo!()
    }
}

async fn start(
    storage: RedisCacheStorage,
    mut handle: HandleStream<InvalidationTopic, Vec<InvalidationRequest>>,
) {
    while let Some(requests) = handle.next().await {
        // FIXME: span over the entire loop
        for request in requests {
            //FIXME: span over one invalidation request
            handle_request(&storage, &request).await;
        }
    }
}

async fn handle_request(storage: &RedisCacheStorage, request: &InvalidationRequest) {
    let keys = get_keys_matching(&storage, &request.key_prefix()).await;
    //FIXME: can we batch deletes with redis pipeline?
    for key in keys {
        storage.delete(key).await;
    }
}

async fn get_keys_matching(storage: &RedisCacheStorage, prefix: &str) -> Vec<String> {
    todo!()
}
