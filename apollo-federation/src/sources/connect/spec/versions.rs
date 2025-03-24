use http::HeaderName;
use http::header;

use super::ConnectSpec;

/// Container for version-specific information
pub(crate) struct VersionInfo {
    pub(crate) allowed_headers: AllowedHeaders,
}

impl VersionInfo {
    fn new(version: &ConnectSpec) -> Self {
        Self {
            allowed_headers: AllowedHeaders::new(version),
        }
    }
}

impl From<ConnectSpec> for VersionInfo {
    fn from(version: ConnectSpec) -> Self {
        Self::new(&version)
    }
}

/// Information about headers that differs between versions
pub(crate) struct AllowedHeaders {
    reserved_headers: Vec<HeaderName>,
    static_headers: Vec<HeaderName>,
}

impl AllowedHeaders {
    pub(crate) fn header_name_is_reserved(&self, header_name: &HeaderName) -> bool {
        self.reserved_headers.contains(header_name)
    }

    pub(crate) fn header_name_allowed_static(&self, header_name: &HeaderName) -> bool {
        self.static_headers.contains(header_name)
    }

    fn new(version: &ConnectSpec) -> Self {
        match version {
            ConnectSpec::V0_1 => Self {
                reserved_headers: vec![
                    header::CONNECTION,
                    header::PROXY_AUTHENTICATE,
                    header::PROXY_AUTHORIZATION,
                    header::TE,
                    header::TRAILER,
                    header::TRANSFER_ENCODING,
                    header::UPGRADE,
                    header::CONTENT_LENGTH,
                    header::CONTENT_ENCODING,
                    header::HOST,
                    header::ACCEPT_ENCODING,
                    KEEP_ALIVE.clone(),
                ],
                static_headers: vec![header::CONTENT_TYPE, header::ACCEPT],
            },
            // moves Host to allow setting it via `value:`
            ConnectSpec::V0_2 => Self {
                reserved_headers: vec![
                    header::CONNECTION,
                    header::PROXY_AUTHENTICATE,
                    header::PROXY_AUTHORIZATION,
                    header::TE,
                    header::TRAILER,
                    header::TRANSFER_ENCODING,
                    header::UPGRADE,
                    header::CONTENT_LENGTH,
                    header::CONTENT_ENCODING,
                    header::ACCEPT_ENCODING,
                    KEEP_ALIVE.clone(),
                ],
                static_headers: vec![header::CONTENT_TYPE, header::ACCEPT, header::HOST],
            },
        }
    }
}

static KEEP_ALIVE: HeaderName = HeaderName::from_static("keep-alive");
