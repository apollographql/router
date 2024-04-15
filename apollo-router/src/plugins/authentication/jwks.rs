use std::collections::HashMap;
use std::collections::HashSet;
use std::mem;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Duration;

use futures::future::join_all;
use futures::future::select;
use futures::future::Either;
use futures::pin_mut;
use futures::stream::repeat;
use futures::stream::select_all;
use http::header::ACCEPT;
use jsonwebtoken::jwk::Jwk;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::Algorithm;
use mime::APPLICATION_JSON;
use serde_json::Value;
use tokio::fs::read_to_string;
use tokio::sync::oneshot;
use tower::BoxError;
use tracing_futures::Instrument;
use url::Url;

use super::Header;
use super::CLIENT;
use super::DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT;

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
    pub(super) poll_interval: Duration,
    pub(super) headers: Vec<Header>,
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
            .map(|JwksConfig { url, headers, .. }| {
                let span = tracing::info_span!("fetch jwks", url = %url);
                get_jwks(url.clone(), headers.clone())
                    .map(|opt_jwks| opt_jwks.map(|jwks| (url, jwks)))
                    .instrument(span)
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
                tokio::time::sleep(config.poll_interval).await;

                if let Some(jwks) = get_jwks(config.url.clone(), config.headers.clone()).await {
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
pub(super) async fn get_jwks(url: Url, headers: Vec<Header>) -> Option<JwkSet> {
    let data = if url.scheme() == "file" {
        let path = url
            .to_file_path()
            .map_err(|e| {
                tracing::error!("url cannot be converted to filesystem path");
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

        let mut builder = my_client
            .get(url)
            .header(ACCEPT, APPLICATION_JSON.essence_str());

        for header in headers.into_iter() {
            builder = builder.header(header.name, header.value);
        }

        builder
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

    let jwks = parse_jwks(&data)?;
    Some(jwks)
}

pub(crate) fn parse_jwks(data: &str) -> Option<JwkSet> {
    // Some JWKS contain algorithms which are not supported by the jsonwebtoken library. That means
    // we can't just deserialize from the retrieved data and proceed. Any unrecognised
    // algorithms will cause deserialization to fail.
    //
    // Try to identify any entries which contain algorithms which are not supported by
    // jsonwebtoken and exclude them
    tracing::debug!(data, "parsing JWKS");

    let mut raw_json: Value = serde_json::from_str(data)
        .map_err(|e| {
            tracing::error!(%e, "could not create JSON Value from url content, enable debug logs to see content");
            e
        })
        .ok()?;

    // remove any keys that can't be parsed
    raw_json.get_mut("keys").and_then(|keys| {
        keys.as_array_mut().map(|array| {
            *array = mem::take(array).into_iter().enumerate().filter(|(index, key)| {
                if let Err(err) = serde_json::from_value::<Jwk>(key.clone()) {
                    let alg = key.get("alg").and_then(|alg|alg.as_str()).unwrap_or("<unknown>");
                    tracing::warn!(%err, alg, index, "ignoring a key since it is not valid, enable debug logs to full content");
                    return false;
                }
                true
            }).map(|(_, key)| key).collect();
        })
    });
    let jwks: JwkSet = serde_json::from_value(raw_json)
        .map_err(|e| {
            tracing::error!(%e, "could not create JWKS from url content, enable debug logs to see content");
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
