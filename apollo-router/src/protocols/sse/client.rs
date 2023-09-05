#![allow(dead_code)]
#![allow(unreachable_pub)]

use std::boxed;
use std::error::Error as StdError;
use std::fmt::Debug;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::{self};
use std::future::Future;
use std::io::ErrorKind;
use std::pin::Pin;
use std::str::FromStr;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;
use std::time::Instant;

use futures::ready;
use futures::Stream;
use http::header::ACCEPT;
use http::header::CACHE_CONTROL;
use hyper::body::HttpBody;
use hyper::client::connect::Connect;
use hyper::client::connect::Connection;
use hyper::client::HttpConnector;
use hyper::client::ResponseFuture;
use hyper::header::HeaderMap;
use hyper::header::HeaderName;
use hyper::header::HeaderValue;
use hyper::service::Service;
use hyper::Body;
use hyper::Request;
use hyper::StatusCode;
use hyper::Uri;
use hyper_rustls::HttpsConnector as RustlsConnector;
use hyper_timeout::TimeoutConnector;
use pin_project_lite::pin_project;
use tokio::io::AsyncRead;
use tokio::io::AsyncWrite;
use tokio::time::Sleep;

use super::config::ReconnectOptions;
use super::error::Error;
use super::error::Result;
use super::event_parser::EventParser;
use super::event_parser::Sse;
use super::retry::BackoffRetry;
use super::retry::RetryStrategy;

pub(crate) type HttpsConnector = RustlsConnector<HttpConnector>;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Represents a [`Pin`]'d [`Send`] + [`Sync`] stream, returned by [`Client`]'s stream method.
pub(crate) type BoxStream<T> = Pin<boxed::Box<dyn Stream<Item = T> + Send + Sync>>;

/// Maximum amount of redirects that the client will follow before
/// giving up, if not overridden via [ClientBuilder::redirect_limit].
const DEFAULT_REDIRECT_LIMIT: u32 = 16;

/// ClientBuilder provides a series of builder methods to easily construct a [`Client`].
pub(crate) struct ClientBuilder {
    url: Uri,
    headers: HeaderMap,
    reconnect_opts: ReconnectOptions,
    read_timeout: Option<Duration>,
    last_event_id: Option<String>,
    method: String,
    body: Option<String>,
    max_redirects: Option<u32>,
}

impl ClientBuilder {
    /// Create a builder for a given URL.
    pub(crate) fn for_url(url: &str) -> Result<ClientBuilder> {
        let url = url
            .parse()
            .map_err(|e| Error::InvalidParameter(Box::new(e)))?;

        let mut header_map = HeaderMap::new();
        header_map.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
        header_map.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));

        Ok(ClientBuilder {
            url,
            headers: header_map,
            reconnect_opts: ReconnectOptions::default(),
            read_timeout: None,
            last_event_id: None,
            method: String::from("GET"),
            max_redirects: None,
            body: None,
        })
    }

    pub(crate) fn get_url(&self) -> &Uri {
        &self.url
    }

    pub(crate) fn get_headers(&self) -> &HeaderMap {
        &self.headers
    }

    pub(crate) fn get_body(&self) -> Option<&str> {
        self.body.as_deref()
    }

    pub(crate) fn headers_mut(&mut self) -> &mut HeaderMap {
        &mut self.headers
    }

    /// Set the request method used for the initial connection to the SSE endpoint.
    pub(crate) fn method(mut self, method: String) -> ClientBuilder {
        self.method = method;
        self
    }

    /// Set the request body used for the initial connection to the SSE endpoint.
    pub(crate) fn body(mut self, body: String) -> ClientBuilder {
        self.body = Some(body);
        self
    }

    /// Set the last event id for a stream when it is created. If it is set, it will be sent to the
    /// server in case it can replay missed events.
    pub(crate) fn last_event_id(mut self, last_event_id: String) -> ClientBuilder {
        self.last_event_id = Some(last_event_id);
        self
    }

    /// Set a HTTP header on the SSE request.
    pub(crate) fn header(mut self, name: &str, value: &str) -> Result<ClientBuilder> {
        let name = HeaderName::from_str(name).map_err(|e| Error::InvalidParameter(Box::new(e)))?;

        let value =
            HeaderValue::from_str(value).map_err(|e| Error::InvalidParameter(Box::new(e)))?;

        self.headers.insert(name, value);
        Ok(self)
    }

    /// Set a read timeout for the underlying connection. There is no read timeout by default.
    pub(crate) fn read_timeout(mut self, read_timeout: Duration) -> ClientBuilder {
        self.read_timeout = Some(read_timeout);
        self
    }

    /// Configure the client's reconnect behaviour according to the supplied
    /// [`ReconnectOptions`].
    ///
    /// [`ReconnectOptions`]: struct.ReconnectOptions.html
    pub(crate) fn reconnect(mut self, opts: ReconnectOptions) -> ClientBuilder {
        self.reconnect_opts = opts;
        self
    }

    /// Customize the client's following behavior when served a redirect.
    /// To disable following redirects, pass `0`.
    /// By default, the limit is [`DEFAULT_REDIRECT_LIMIT`].
    pub(crate) fn redirect_limit(mut self, limit: u32) -> ClientBuilder {
        self.max_redirects = Some(limit);
        self
    }

    /// Build with a specific client connector.
    pub(crate) fn build_with_conn<C>(self, conn: C) -> Client<TimeoutConnector<C>>
    where
        C: Service<Uri> + Clone + Send + Sync + 'static,
        C::Response: Connection + AsyncRead + AsyncWrite + Send + Unpin,
        C::Future: Send + 'static,
        C::Error: Into<BoxError>,
    {
        let mut connector = TimeoutConnector::new(conn);
        connector.set_read_timeout(self.read_timeout);

        let client = hyper::Client::builder().build::<_, hyper::Body>(connector);

        Client {
            http: client,
            request_props: RequestProps {
                url: self.url,
                headers: self.headers,
                method: self.method,
                body: self.body,
                reconnect_opts: self.reconnect_opts,
                max_redirects: self.max_redirects.unwrap_or(DEFAULT_REDIRECT_LIMIT),
            },
            last_event_id: self.last_event_id,
        }
    }

    /// Build with an HTTP client connector.
    pub(crate) fn build_http(self) -> Client<TimeoutConnector<HttpConnector>> {
        self.build_with_conn(HttpConnector::new())
    }

    /// Build with an HTTPS client connector, using the OS root certificate store.
    pub(crate) fn build(self) -> Client<TimeoutConnector<HttpsConnector>> {
        let conn = hyper_rustls::HttpsConnectorBuilder::new()
            .with_native_roots()
            .https_or_http()
            .enable_http2()
            .build();
        self.build_with_conn(conn)
    }

    /// Build with the given [`hyper::client::Client`].
    pub(crate) fn build_with_http_client<C>(self, http: hyper::Client<C>) -> Client<C>
    where
        C: Connect + Clone + Send + Sync + 'static,
    {
        Client {
            http,
            request_props: RequestProps {
                url: self.url,
                headers: self.headers,
                method: self.method,
                body: self.body,
                reconnect_opts: self.reconnect_opts,
                max_redirects: self.max_redirects.unwrap_or(DEFAULT_REDIRECT_LIMIT),
            },
            last_event_id: self.last_event_id,
        }
    }
}

#[derive(Clone)]
struct RequestProps {
    url: Uri,
    headers: HeaderMap,
    method: String,
    body: Option<String>,
    reconnect_opts: ReconnectOptions,
    max_redirects: u32,
}

/// A client implementation that connects to a server using the Server-Sent Events protocol
/// and consumes the event stream indefinitely.
/// Can be parameterized with different hyper Connectors, such as HTTP or HTTPS.
pub(crate) struct Client<C> {
    http: hyper::Client<C>,
    request_props: RequestProps,
    last_event_id: Option<String>,
}

impl<C> Client<C>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    /// Connect to the server and begin consuming the stream. Produces a
    /// [`Stream`] of [`Event`](crate::Event)s wrapped in [`Result`].
    ///
    /// Do not use the stream after it returned an error!
    ///
    /// After the first successful connection, the stream will
    /// reconnect for retryable errors.
    pub(crate) fn stream(&self) -> BoxStream<Result<Sse>> {
        Box::pin(ReconnectingRequest::new(
            self.http.clone(),
            self.request_props.clone(),
            self.last_event_id.clone(),
        ))
    }
}

pin_project! {

    #[project = StateProj]
    #[allow(clippy::large_enum_variant)] // false positive
    enum State {
        New,
        Connecting {
            retry: bool,
            #[pin]
            resp: ResponseFuture,
        },
        Connected {
            #[pin]
            body: hyper::Body
        },
        WaitingToReconnect {
            #[pin]
            sleep: Sleep
        },
        FollowingRedirect {
            redirect: Option<HeaderValue>
        },
        StreamClosed,
    }
}

impl State {
    fn as_str(&self) -> &'static str {
        match self {
            State::New => "new",
            State::Connecting { retry: false, .. } => "connecting(no-retry)",
            State::Connecting { retry: true, .. } => "connecting(retry)",
            State::Connected { .. } => "connected",
            State::WaitingToReconnect { .. } => "waiting-to-reconnect",
            State::FollowingRedirect { .. } => "following-redirect",
            State::StreamClosed => "closed",
        }
    }
}

impl Debug for State {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

pin_project! {
    #[must_use = "streams do nothing unless polled"]
    pub(crate) struct ReconnectingRequest<C> {
        http: hyper::Client<C>,
        props: RequestProps,
        #[pin]
        state: State,
        retry_strategy: Box<dyn RetryStrategy + Send + Sync>,
        current_url: Uri,
        redirect_count: u32,
        event_parser: EventParser,
        last_event_id: Option<String>,
    }
}

impl<C> ReconnectingRequest<C> {
    fn new(
        http: hyper::Client<C>,
        props: RequestProps,
        last_event_id: Option<String>,
    ) -> ReconnectingRequest<C> {
        let reconnect_delay = props.reconnect_opts.delay;
        let delay_max = props.reconnect_opts.delay_max;
        let backoff_factor = props.reconnect_opts.backoff_factor;

        let url = props.url.clone();
        ReconnectingRequest {
            props,
            http,
            state: State::New,
            retry_strategy: Box::new(BackoffRetry::new(
                reconnect_delay,
                delay_max,
                backoff_factor,
                true,
            )),
            redirect_count: 0,
            current_url: url,
            event_parser: EventParser::new(),
            last_event_id,
        }
    }

    fn send_request(&self) -> Result<ResponseFuture>
    where
        C: Connect + Clone + Send + Sync + 'static,
    {
        let mut request_builder = Request::builder()
            .method(self.props.method.as_str())
            .uri(&self.current_url);

        for (name, value) in &self.props.headers {
            request_builder = request_builder.header(name, value);
        }

        if let Some(id) = self.last_event_id.as_ref() {
            if !id.is_empty() {
                let id_as_header =
                    HeaderValue::from_str(id).map_err(|e| Error::InvalidParameter(Box::new(e)))?;

                request_builder = request_builder.header("last-event-id", id_as_header);
            }
        }

        let body = match &self.props.body {
            Some(body) => Body::from(body.to_string()),
            None => Body::empty(),
        };

        let request = request_builder
            .body(body)
            .map_err(|e| Error::InvalidParameter(Box::new(e)))?;

        Ok(self.http.request(request))
    }

    fn reset_redirects(self: Pin<&mut Self>) {
        let url = self.props.url.clone();
        let this = self.project();
        *this.current_url = url;
        *this.redirect_count = 0;
    }

    fn increment_redirect_counter(self: Pin<&mut Self>) -> bool {
        if self.redirect_count == self.props.max_redirects {
            return false;
        }
        *self.project().redirect_count += 1;
        true
    }
}

impl<C> Stream for ReconnectingRequest<C>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    type Item = Result<Sse>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        tracing::trace!("ReconnectingRequest::poll({:?})", &self.state);

        loop {
            let this = self.as_mut().project();
            if let Some(event) = this.event_parser.get_event() {
                return match event {
                    Sse::Event(ref evt) => {
                        *this.last_event_id = evt.id.clone();

                        if let Some(retry) = evt.retry {
                            this.retry_strategy
                                .change_base_delay(Duration::from_millis(retry));
                        }
                        Poll::Ready(Some(Ok(event)))
                    }
                    Sse::Comment(_) => Poll::Ready(Some(Ok(event))),
                };
            }

            tracing::trace!("ReconnectingRequest::poll loop({:?})", &this.state);

            let state = this.state.project();

            match state {
                StateProj::StreamClosed => return Poll::Ready(Some(Err(Error::StreamClosed))),
                // New immediately transitions to Connecting, and exists only
                // to ensure that we only connect when polled.
                StateProj::New => {
                    *self.as_mut().project().event_parser = EventParser::new();
                    match self.send_request() {
                        Ok(resp) => {
                            let retry = self.props.reconnect_opts.retry_initial;
                            self.as_mut()
                                .project()
                                .state
                                .set(State::Connecting { resp, retry })
                        }
                        Err(e) => {
                            // This error seems to be unrecoverable. So we should just shut down the
                            // stream.
                            self.as_mut().project().state.set(State::StreamClosed);
                            return Poll::Ready(Some(Err(e)));
                        }
                    }
                }
                StateProj::Connecting { retry, resp } => match ready!(resp.poll(cx)) {
                    Ok(resp) => {
                        tracing::debug!("HTTP response: {:#?}", resp);

                        if resp.status().is_success() {
                            self.as_mut().project().retry_strategy.reset(Instant::now());
                            self.as_mut().reset_redirects();
                            self.as_mut().project().state.set(State::Connected {
                                body: resp.into_body(),
                            });
                            continue;
                        }

                        if resp.status() == StatusCode::MOVED_PERMANENTLY
                            || resp.status() == StatusCode::TEMPORARY_REDIRECT
                        {
                            tracing::debug!("got redirected ({})", resp.status());

                            if self.as_mut().increment_redirect_counter() {
                                tracing::debug!("following redirect {}", self.redirect_count);

                                self.as_mut().project().state.set(State::FollowingRedirect {
                                    redirect: resp.headers().get(hyper::header::LOCATION).cloned(),
                                });
                                continue;
                            } else {
                                tracing::debug!(
                                    "redirect limit reached ({})",
                                    self.props.max_redirects
                                );

                                self.as_mut().project().state.set(State::StreamClosed);
                                return Poll::Ready(Some(Err(Error::MaxRedirectLimitReached(
                                    self.props.max_redirects,
                                ))));
                            }
                        }

                        self.as_mut().reset_redirects();
                        self.as_mut().project().state.set(State::New);
                        return Poll::Ready(Some(Err(Error::UnexpectedResponse(resp.status()))));
                    }
                    Err(e) => {
                        // This seems basically impossible. AFAIK we can only get this way if we
                        // poll after it was already ready
                        tracing::warn!("request returned an error: {}", e);
                        if !*retry {
                            self.as_mut().project().state.set(State::New);
                            return Poll::Ready(Some(Err(Error::HttpStream(Box::new(e)))));
                        }
                        let duration = self
                            .as_mut()
                            .project()
                            .retry_strategy
                            .next_delay(Instant::now());
                        self.as_mut()
                            .project()
                            .state
                            .set(State::WaitingToReconnect {
                                sleep: delay(duration, "retrying"),
                            })
                    }
                },
                StateProj::FollowingRedirect {
                    redirect: maybe_header,
                } => match uri_from_header(maybe_header) {
                    Ok(uri) => {
                        *self.as_mut().project().current_url = uri;
                        self.as_mut().project().state.set(State::New);
                    }
                    Err(e) => {
                        self.as_mut().project().state.set(State::StreamClosed);
                        return Poll::Ready(Some(Err(e)));
                    }
                },
                StateProj::Connected { body } => match ready!(body.poll_data(cx)) {
                    Some(Ok(result)) => {
                        this.event_parser.process_bytes(result)?;
                        continue;
                    }
                    Some(Err(e)) => {
                        if self.props.reconnect_opts.reconnect {
                            let duration = self
                                .as_mut()
                                .project()
                                .retry_strategy
                                .next_delay(Instant::now());
                            self.as_mut()
                                .project()
                                .state
                                .set(State::WaitingToReconnect {
                                    sleep: delay(duration, "reconnecting"),
                                });
                        }

                        if let Some(cause) = e.source() {
                            if let Some(downcast) = cause.downcast_ref::<std::io::Error>() {
                                if let std::io::ErrorKind::TimedOut = downcast.kind() {
                                    return Poll::Ready(Some(Err(Error::TimedOut)));
                                }
                            }
                        } else {
                            return Poll::Ready(Some(Err(Error::HttpStream(Box::new(e)))));
                        }
                    }
                    None => {
                        let duration = self
                            .as_mut()
                            .project()
                            .retry_strategy
                            .next_delay(Instant::now());
                        self.as_mut()
                            .project()
                            .state
                            .set(State::WaitingToReconnect {
                                sleep: delay(duration, "retrying"),
                            });

                        if self.event_parser.was_processing() {
                            return Poll::Ready(Some(Err(Error::UnexpectedEof)));
                        }
                        return Poll::Ready(Some(Err(Error::Eof)));
                    }
                },
                StateProj::WaitingToReconnect { sleep: delay } => {
                    ready!(delay.poll(cx));
                    tracing::info!(url = ?self.current_url, "Reconnecting sse connection");
                    self.as_mut().project().state.set(State::New);
                }
            };
        }
    }
}

fn uri_from_header(maybe_header: &Option<HeaderValue>) -> Result<Uri> {
    let header = maybe_header.as_ref().ok_or_else(|| {
        Error::MalformedLocationHeader(Box::new(std::io::Error::new(
            ErrorKind::NotFound,
            "missing Location header",
        )))
    })?;

    let header_string = header
        .to_str()
        .map_err(|e| Error::MalformedLocationHeader(Box::new(e)))?;

    header_string
        .parse::<Uri>()
        .map_err(|e| Error::MalformedLocationHeader(Box::new(e)))
}

fn delay(dur: Duration, description: &str) -> Sleep {
    tracing::debug!("Waiting {:?} before {}", dur, description);
    tokio::time::sleep(dur)
}

#[derive(Debug)]
struct StatusError {
    status: StatusCode,
}

impl Display for StatusError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "Invalid status code: {}", self.status)
    }
}

impl std::error::Error for StatusError {}
