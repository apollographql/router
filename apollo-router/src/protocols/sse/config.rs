use std::time::Duration;

/// Configuration for a [`Client`]'s reconnect behaviour.
#[derive(Clone, Debug)]
pub(crate) struct ReconnectOptions {
    pub(crate) retry_initial: bool,
    pub(crate) reconnect: bool,
    pub(crate) delay: Duration,
    pub(crate) backoff_factor: u32,
    pub(crate) delay_max: Duration,
    pub(crate) timeout: Duration,
}

impl ReconnectOptions {
    /// Start building a `ReconnectOptions`, by enabling or disabling
    /// reconnection on stream error.
    ///
    /// If `reconnect` is `true` (the [default]), the client will automatically
    /// try to reconnect if the stream ends due to an error. If it is `false`,
    /// the client will stop receiving events after an error.
    ///
    /// [default]: #method.default
    pub(crate) fn reconnect(reconnect: bool) -> ReconnectOptionsBuilder {
        ReconnectOptionsBuilder::new(reconnect)
    }
}

// FIXME: Do not set default values here, but set them in the subscrption plugin config directly
impl Default for ReconnectOptions {
    /// The default reconnect behaviour is to automatically try to reconnect if
    /// the stream ends due to an error, but not to retry if the initial
    /// connection fails.
    ///
    /// The client will wait before each reconnect attempt, to allow time for
    /// the error condition to be resolved (e.g. for the SSE server to restart
    /// if it went down). It will wait 1 second before the first attempt, and
    /// then back off exponentially, up to a maximum wait of 1 minute.
    fn default() -> ReconnectOptions {
        ReconnectOptions {
            retry_initial: false,
            reconnect: true,
            delay: Duration::from_secs(1),
            backoff_factor: 2,
            delay_max: Duration::from_secs(60),
            timeout: Duration::from_secs(60),
        }
    }
}

/// Builder for [`ReconnectOptions`].
///
/// [`ReconnectOptions`]: struct.ReconnectOptions.html
pub(crate) struct ReconnectOptionsBuilder {
    opts: ReconnectOptions,
}

impl ReconnectOptionsBuilder {
    pub(crate) fn new(reconnect: bool) -> Self {
        let opts = ReconnectOptions {
            reconnect,
            ..Default::default()
        };
        Self { opts }
    }

    /// Configure whether to retry if the initial connection to the server
    /// fails.
    ///
    /// If `true`, the client will automatically retry the connection, with the
    /// same delay and backoff behaviour as for reconnects due to stream error.
    /// If `false` (the [default]), the client will not retry the initial
    /// connection.
    ///
    /// [default]: struct.ReconnectOptions.html#method.default
    pub(crate) fn retry_initial(mut self, retry: bool) -> Self {
        self.opts.retry_initial = retry;
        self
    }

    /// Configure the initial delay before trying to reconnect (the [default] is
    /// 1 second).
    ///
    /// After an error, the client will wait this long before the first attempt
    /// to reconnect.  Subsequent reconnect attempts may wait longer, depending
    /// on the [`backoff_factor`].
    ///
    /// [default]: struct.ReconnectOptions.html#method.default
    /// [`backoff_factor`]: #method.backoff_factor
    pub(crate) fn delay(mut self, delay: Duration) -> Self {
        self.opts.delay = delay;
        self
    }

    /// Configure the factor by which delays between reconnect attempts will
    /// exponentially increase, up to [`delay_max`]. The [default] factor is 2,
    /// so each reconnect attempt will wait twice as long as the previous one.
    ///
    /// Set this to 1 to disable exponential backoff (i.e. to make reconnect
    /// attempts at regular intervals equal to the configured [`delay`]).
    ///
    /// [`delay_max`]: #method.delay_max
    /// [default]: struct.ReconnectOptions.html#method.default
    /// [`delay`]: #method.delay
    pub(crate) fn backoff_factor(mut self, factor: u32) -> Self {
        self.opts.backoff_factor = factor;
        self
    }

    /// Configure the maximum delay between reconnects (the [default] is 1
    /// minute). The exponential backoff configured by [`backoff_factor`] will
    /// not cause a delay greater than this value.
    ///
    /// [default]: struct.ReconnectOptions.html#method.default
    /// [`backoff_factor`]: #method.backoff_factor
    pub(crate) fn delay_max(mut self, max: Duration) -> Self {
        self.opts.delay_max = max;
        self
    }

    /// Configure the maximum time it will wait to get a successful connection
    /// before giving up. The [default] is 30 minutes.
    pub(crate) fn timeout(mut self, max: Duration) -> Self {
        self.opts.timeout = max;
        self
    }

    /// Finish building the `ReconnectOptions`.
    pub(crate) fn build(self) -> ReconnectOptions {
        self.opts
    }
}
