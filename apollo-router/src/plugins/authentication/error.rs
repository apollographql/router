use displaydoc::Display;
use thiserror::Error;
use jsonwebtoken::errors::Error as JWTError;
use tower::BoxError;
use jsonwebtoken::Algorithm;
use jsonwebtoken::jwk::KeyAlgorithm;

#[derive(Debug, Display, Error)]
pub(crate) enum AuthenticationError<'a> {
    /// Configured header is not convertible to a string
    CannotConvertToString,

    /// Header Value: '{0}' is not correctly formatted. prefix should be '{1}'
    InvalidPrefix(&'a str, &'a str),

    /// Header Value: '{0}' is not correctly formatted. Missing JWT
    MissingJWT(&'a str),

    /// '{0}' is not a valid JWT header: {1}
    InvalidHeader(&'a str, JWTError),

    /// Cannot create decoding key: {0}
    CannotCreateDecodingKey(JWTError),

    /// JWK does not contain an algorithm
    JWKHasNoAlgorithm,

    /// Cannot decode JWT: {0}
    CannotDecodeJWT(JWTError),

    /// Cannot insert claims into context: {0}
    CannotInsertClaimsIntoContext(BoxError),

    /// Cannot find kid: '{0:?}' in JWKS list
    CannotFindKID(Option<String>),

    /// Cannot find a suitable key for: alg: '{0:?}', kid: '{1:?}' in JWKS list
    CannotFindSuitableKey(Algorithm, Option<String>),

    /// Invalid issuer: the token's `iss` was '{token}', but signed with a key from '{expected}'
    InvalidIssuer { expected: String, token: String },

    /// Unsupported key algorithm: {0}
    UnsupportedKeyAlgorithm(KeyAlgorithm),
}

impl<'a> AuthenticationError<'a> {
    pub(crate) fn as_context_object(&self) -> serde_json_bytes::Value {
        let (code, reason) = match self {
            // TODO What should we put for the reason?
            AuthenticationError::CannotConvertToString => {
                ("CANNOT_CONVERT_TO_STRING", "")
            }
            AuthenticationError::InvalidPrefix(_, _) => {
                ("INVALID_PREFIX", "")
            }
            AuthenticationError::MissingJWT(_) => {
                ("MISSING_JWT", "")
            }
            AuthenticationError::InvalidHeader(_, _) => {
                ("INVALID_HEADER", "")
            }
            AuthenticationError::CannotCreateDecodingKey(_) => {
                ("CANNOT_CREATE_DECODING_KEY", "")
            }
            AuthenticationError::JWKHasNoAlgorithm => {
                ("JWK_HAS_NO_ALGORITHM", "")
            }
            AuthenticationError::CannotDecodeJWT(_) => {
                ("CANNOT_DECODE_JWT", "")
            }
            AuthenticationError::CannotInsertClaimsIntoContext(_) => {
                ("CANNOT_INSERT_CLAIMS_INTO_CONTEXT", "")
            }
            AuthenticationError::CannotFindKID(_) => {
                ("CANNOT_FIND_KID", "")
            }
            AuthenticationError::CannotFindSuitableKey(_, _) => {
                ("CANNOT_FIND_SUITABLE_KEY", "")
            }
            AuthenticationError::InvalidIssuer { .. } => {
                ("INVALID_ISSUER", "")
            }
            AuthenticationError::UnsupportedKeyAlgorithm(_) => {
                ("UNSUPPORTED_KEY_ALGORITHM", "")
            }
        };

        serde_json_bytes::json!({
            "message": self.to_string(),
            "code": code,
            "reason": reason
        })
    }
}

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("header_value_prefix must not contain whitespace")]
    BadHeaderValuePrefix,
}