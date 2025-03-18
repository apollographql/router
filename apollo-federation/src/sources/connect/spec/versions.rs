use http::HeaderName;
use http::header;

use super::ConnectSpec;

pub(crate) trait SpecVersionConfig {
    fn header_name_is_reserved(&self, header_name: &HeaderName) -> bool;
    fn header_name_allowed_static(&self, header_name: &HeaderName) -> bool;
}

impl SpecVersionConfig for ConnectSpec {
    /// These headers are not allowed to be defined by connect directives at all.
    /// Copied from Router's plugins::headers
    /// Headers from https://datatracker.ietf.org/doc/html/rfc2616#section-13.5.1
    /// These are not propagated by default using a regex match as they will not make sense for the
    /// second hop.
    /// In addition, because our requests are not regular proxy requests content-type, content-length
    /// and host are also in the exclude list.
    fn header_name_is_reserved(&self, header_name: &HeaderName) -> bool {
        static KEEP_ALIVE: HeaderName = HeaderName::from_static("keep-alive");
        match self {
            ConnectSpec::V0_1 => {
                matches!(
                    *header_name,
                    header::CONNECTION
                        | header::PROXY_AUTHENTICATE
                        | header::PROXY_AUTHORIZATION
                        | header::TE
                        | header::TRAILER
                        | header::TRANSFER_ENCODING
                        | header::UPGRADE
                        | header::CONTENT_LENGTH
                        | header::CONTENT_ENCODING
                        | header::HOST
                        | header::ACCEPT_ENCODING
                ) || header_name == KEEP_ALIVE
            } // TODO V0_2: allow host
        }
    }

    /// These headers can be defined as static values in connect directives, but can't be
    /// forwarded by the user.
    fn header_name_allowed_static(&self, header_name: &HeaderName) -> bool {
        match self {
            ConnectSpec::V0_1 => matches!(*header_name, header::CONTENT_TYPE | header::ACCEPT),
            // TODO V0_2: allow host
        }
    }
}
