// With regards to ELv2 licensing, this entire file is license key functionality

use futures::future::join_all;
use futures::future::select;
use futures::future::Either;
use futures::pin_mut;
use futures::stream::repeat;
use futures::stream::select_all;
use http::header::ACCEPT;
use http::header::CONTENT_TYPE;
use jsonwebtoken::jwk::JwkSet;
use mime::APPLICATION_JSON;
use tokio::fs::read_to_string;
use tokio::sync::oneshot;
use url::Url;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

#[cfg(not(test))]
use crate::error::LicenseError;
use crate::plugins::authentication::DEFAULT_AUTHENTICATION_DOWNLOAD_INTERVAL;
#[cfg(not(test))]
use crate::services::apollo_graph_reference;

use super::{CLIENT, DEFAULT_AUTHENTICATION_NETWORK_TIMEOUT};

#[derive(Clone)]
pub(super) struct JwksManager {
    urls: Vec<Url>,
    jwks_map: Arc<Mutex<HashMap<Url, Option<JwkSet>>>>,
    _drop_signal: Arc<oneshot::Sender<()>>,
}

impl JwksManager {
    pub(super) async fn new(urls: Vec<Url>) -> Self {
        use futures::FutureExt;

        let downloads = urls
            .iter()
            .cloned()
            .map(|url| get_jwks(url.clone()).map(|jwks| (url, jwks)))
            .collect::<Vec<_>>();

        let jwks_map = Arc::new(Mutex::new(join_all(downloads).await.into_iter().collect()));

        let (_drop_signal, drop_receiver) = oneshot::channel::<()>();

        tokio::task::spawn(poll(urls.clone(), jwks_map.clone(), drop_receiver));

        JwksManager {
            urls,
            jwks_map,
            _drop_signal: Arc::new(_drop_signal),
        }
    }

    pub(super) fn iter_jwks<'a>(&'a self) -> impl Iterator<Item = JwkSet> + 'a {
        Iter {
            urls: self.urls.clone(),
            manager: &self,
        }
    }
}

async fn poll(
    urls: Vec<Url>,
    jwks_map: Arc<Mutex<HashMap<Url, Option<JwkSet>>>>,
    drop_receiver: oneshot::Receiver<()>,
) {
    use futures::stream::StreamExt;

    let mut streams = select_all(urls.into_iter().map(move |url| {
        let jwks_map = jwks_map.clone();
        Box::pin(repeat((url, jwks_map)).then(|(url, jwks_map)| async move {
            tokio::time::sleep(DEFAULT_AUTHENTICATION_DOWNLOAD_INTERVAL).await;

            let opt_jwks = get_jwks(url.clone()).await;
            {
                if let Ok(mut map) = jwks_map.lock() {
                    map.insert(url, opt_jwks);
                }
            }
        }))
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
        #[cfg(not(test))]
        apollo_graph_reference()
            .ok_or(LicenseError::MissingGraphReference)
            .map_err(|e| {
                tracing::error!(%e, "could not activate authentication feature");
                e
            })
            .ok()?;
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
    let jwks: JwkSet = serde_json::from_str(&data)
        .map_err(|e| {
            tracing::error!(%e, "could not create JWKS from url content");
            e
        })
        .ok()?;
    Some(jwks)
}

struct Iter<'a> {
    manager: &'a JwksManager,
    urls: Vec<Url>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = JwkSet;

    fn next(&mut self) -> Option<Self::Item> {
        match self.urls.pop() {
            None => None,
            Some(url) => {
                if let Ok(map) = self.manager.jwks_map.lock() {
                    map.get(&url)?.clone()
                } else {
                    None
                }
            }
        }
    }
}
