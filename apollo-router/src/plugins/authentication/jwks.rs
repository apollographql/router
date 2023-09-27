use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::RwLock;

use futures::future::join_all;
use futures::future::select;
use futures::future::Either;
use futures::pin_mut;
use futures::stream::repeat;
use futures::stream::select_all;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::Algorithm;
use mime::APPLICATION_JSON;
use serde_json::Value;
use tokio::fs::read_to_string;
use tokio::sync::oneshot;
use tower::BoxError;
use url::Url;

use super::CLIENT;
use super::DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT;
use crate::plugins::authentication::DEFAULT_AUTHENTICATION_DOWNLOAD_INTERVAL;

#[derive(Clone)]
pub(super) struct JwksManager {
    list: Vec<JwksConfig>,
    jwks_map: Arc<RwLock<HashMap<Url, JwkSet>>>,
    _drop_signal: Arc<oneshot::Sender<()>>,
}

#[derive(Clone)]
pub(super) struct JwksConfig {
    pub(super) url: Url,
    pub(super) issuer: Option<String>,
    pub(super) algorithms: Option<HashSet<Algorithm>>,
}

#[derive(Clone)]
pub(super) struct JwkSetInfo {
    pub(super) jwks: JwkSet,
    pub(super) issuer: Option<String>,
    pub(super) algorithms: Option<HashSet<Algorithm>>,
}

impl JwksManager {
    pub(super) async fn new(list: Vec<JwksConfig>) -> Result<Self, BoxError> {
        use futures::FutureExt;

        let downloads = list
            .iter()
            .cloned()
            .map(|JwksConfig { url, .. }| {
                get_jwks(url.clone()).map(|opt_jwks| opt_jwks.map(|jwks| (url, jwks)))
            })
            .collect::<Vec<_>>();

        let jwks_map: HashMap<_, _> = join_all(downloads).await.into_iter().flatten().collect();

        let jwks_map = Arc::new(RwLock::new(jwks_map));
        let (_drop_signal, drop_receiver) = oneshot::channel::<()>();

        tokio::task::spawn(poll(list.clone(), jwks_map.clone(), drop_receiver));

        Ok(JwksManager {
            list,
            jwks_map,
            _drop_signal: Arc::new(_drop_signal),
        })
    }

    #[cfg(test)]
    pub(super) fn new_test(list: Vec<JwksConfig>, jwks: HashMap<Url, JwkSet>) -> Self {
        let (_drop_signal, _) = oneshot::channel::<()>();

        JwksManager {
            list,
            jwks_map: Arc::new(RwLock::new(jwks)),
            _drop_signal: Arc::new(_drop_signal),
        }
    }

    pub(super) fn iter_jwks(&self) -> Iter {
        Iter {
            list: self.list.clone(),
            manager: self,
        }
    }
}

async fn poll(
    list: Vec<JwksConfig>,
    jwks_map: Arc<RwLock<HashMap<Url, JwkSet>>>,
    drop_receiver: oneshot::Receiver<()>,
) {
    use futures::stream::StreamExt;

    let mut streams = select_all(list.into_iter().map(move |config| {
        let jwks_map = jwks_map.clone();
        Box::pin(
            repeat((config, jwks_map)).then(|(config, jwks_map)| async move {
                tokio::time::sleep(DEFAULT_AUTHENTICATION_DOWNLOAD_INTERVAL).await;

                if let Some(jwks) = get_jwks(config.url.clone()).await {
                    if let Ok(mut map) = jwks_map.write() {
                        map.insert(config.url, jwks);
                    }
                }
            }),
        )
    }));

    pin_mut!(drop_receiver);

    loop {
        let next = streams.next();
        pin_mut!(next);

        match select(drop_receiver, next).await {
            // the _drop_signal was dropped, we must shut down the task
            Either::Left((_res, _)) => return,
            // another JWKS download was performed
            Either::Right((Some(()), receiver)) => {
                drop_receiver = receiver;
            }
            Either::Right((None, _)) => return,
        };
    }
}

// This function is expected to return an Optional value, but we'd like to let
// users know the various failure conditions. Hence the various clumsy map_err()
// scattered through the processing.
pub(super) async fn get_jwks(url: Url) -> Option<JwkSet> {
    let data = if url.scheme() == "file" {
        let path = url
            .to_file_path()
            .map_err(|e| {
                tracing::error!("could not process url: {:?}", url);
                e
            })
            .ok()?;
        read_to_string(path)
            .await
            .map_err(|e| {
                tracing::error!(%e, "could not read JWKS path");
                e
            })
            .ok()?
    } else {
        let my_client = CLIENT
            .as_ref()
            .map_err(|e| {
                tracing::error!(%e, "could not activate authentication feature");
                e
            })
            .ok()?
            .clone();

        my_client
            .get(url)
            .header(ACCEPT, APPLICATION_JSON.essence_str())
            .header(CONTENT_TYPE, APPLICATION_JSON.essence_str())
            .timeout(DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT)
            .send()
            .await
            .map_err(|e| {
                tracing::error!(%e, "could not get url");
                e
            })
            .ok()?
            .text()
            .await
            .map_err(|e| {
                tracing::error!(%e, "could not process url content");
                e
            })
            .ok()?
    };
    // Some JWKS contain algorithms which are not supported by the jsonwebtoken library. That means
    // we can't just deserialize from the retrieved data and proceed. Any unrecognised
    // algorithms will cause deserialization to fail.
    //
    // The only known failing case right now is "ES512". To accomodate this, we create a Value from
    // the retrieved JWKS data. We then process this to discard any entries with an algorithm of "ES512".
    //
    // We always print a WARN to let people know that if their JWKS contains a key with an alg of
    // ES512 we will not be using it.
    //
    // We may need to continue to update this over time as the jsonwebtoken library evolves or if
    // other problematic algorithms are identified.

    tracing::warn!(
        "If your JWKS contains keys with an 'alg' of 'ES512', they are ignored by the router."
    );
    let mut raw_json: Value = serde_json::from_str(&data)
        .map_err(|e| {
            tracing::error!(%e, "could not create JSON Value from url content");
            e
        })
        .ok()?;
    let ignore_alg = serde_json::json!("ES512");
    raw_json.get_mut("keys").and_then(|keys| {
        keys.as_array_mut().map(|array| {
            array.retain(|row| row.get("alg").unwrap_or_else(|| &Value::Null) != &ignore_alg)
        })
    });
    let jwks: JwkSet = serde_json::from_value(raw_json)
        .map_err(|e| {
            tracing::error!(%e, "could not create JWKS from url content");
            e
        })
        .ok()?;
    Some(jwks)
}

pub(super) struct Iter<'a> {
    manager: &'a JwksManager,
    list: Vec<JwksConfig>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = JwkSetInfo;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.list.pop() {
                None => return None,
                Some(config) => {
                    if let Ok(map) = self.manager.jwks_map.read() {
                        if let Some(jwks) = map.get(&config.url) {
                            return Some(JwkSetInfo {
                                jwks: jwks.clone(),
                                issuer: config.issuer.clone(),
                                algorithms: config.algorithms.clone(),
                            });
                        }
                    } else {
                        return None;
                    }
                }
            }
        }
    }
}
