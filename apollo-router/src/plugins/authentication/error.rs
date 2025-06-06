use displaydoc::Display;
use jsonwebtoken::Algorithm;
use jsonwebtoken::errors::Error as JWTError;
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::jwk::KeyAlgorithm;
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;
use tower::BoxError;

#[derive(Debug, Display, Error)]
pub(crate) enum AuthenticationError {
    /// Configured header is not convertible to a string
    CannotConvertToString,

    /// Value of '{0}' JWT header should be prefixed with '{1}'
    InvalidJWTPrefix(String, String),

    /// Value of '{0}' JWT header has only '{1}' prefix but no JWT token
    MissingJWTToken(String, String),

    /// '{0}' is not a valid JWT header: {1}
    InvalidHeader(String, JWTError),

    /// Cannot create decoding key: {0}
    CannotCreateDecodingKey(JWTError),

    /// JWK does not contain an algorithm
    JWKHasNoAlgorithm,

    /// Cannot decode JWT: {0}
    CannotDecodeJWT(JWTError),

    /// Cannot insert claims into context: {0}
    CannotInsertClaimsIntoContext(BoxError),

    /// Cannot find kid: '{0:?}' in JWKS list
    CannotFindKID(String),

    /// Cannot find a suitable key for: alg: '{0:?}', kid: '{1:?}' in JWKS list
    CannotFindSuitableKey(Algorithm, Option<String>),

    /// Invalid issuer: the token's `iss` was '{token}', but signed with a key from JWKS configured to only accept from '{expected}'
    InvalidIssuer { expected: String, token: String },

    /// Invalid audience: the token's `aud` was '{actual}', but '{expected}' was expected
    InvalidAudience { actual: String, expected: String },

    /// Unsupported key algorithm: {0}
    UnsupportedKeyAlgorithm(KeyAlgorithm),
}

fn jwt_error_to_reason(jwt_err: &JWTError) -> &'static str {
    let kind = jwt_err.kind();
    match kind {
        ErrorKind::InvalidToken => "INVALID_TOKEN",
        ErrorKind::InvalidSignature => "INVALID_SIGNATURE",
        ErrorKind::InvalidEcdsaKey => "INVALID_ECDSA_KEY",
        ErrorKind::InvalidRsaKey(_) => "INVALID_RSA_KEY",
        ErrorKind::RsaFailedSigning => "RSA_FAILED_SIGNING",
        ErrorKind::InvalidAlgorithmName => "INVALID_ALGORITHM_NAME",
        ErrorKind::InvalidKeyFormat => "INVALID_KEY_FORMAT",
        ErrorKind::MissingRequiredClaim(_) => "MISSING_REQUIRED_CLAIM",
        ErrorKind::ExpiredSignature => "EXPIRED_SIGNATURE",
        ErrorKind::InvalidIssuer => "INVALID_ISSUER",
        ErrorKind::InvalidAudience => "INVALID_AUDIENCE",
        ErrorKind::InvalidSubject => "INVALID_SUBJECT",
        ErrorKind::ImmatureSignature => "IMMATURE_SIGNATURE",
        ErrorKind::InvalidAlgorithm => "INVALID_ALGORITHM",
        ErrorKind::MissingAlgorithm => "MISSING_ALGORITHM",
        ErrorKind::Base64(_) => "BASE64_ERROR",
        ErrorKind::Json(_) => "JSON_ERROR",
        ErrorKind::Utf8(_) => "UTF8_ERROR",
        ErrorKind::Crypto(_) => "CRYPTO_ERROR",
        // ErrorKind is non-exhaustive
        _ => "UNKNOWN_ERROR",
    }
}

impl AuthenticationError {
    pub(crate) fn as_context_object(&self) -> ErrorContext {
        let (code, reason) = match self {
            AuthenticationError::CannotConvertToString => ("CANNOT_CONVERT_TO_STRING", None),
            AuthenticationError::InvalidJWTPrefix(_, _) => ("INVALID_PREFIX", None),
            AuthenticationError::MissingJWTToken(_, _) => ("MISSING_JWT", None),
            AuthenticationError::InvalidHeader(_, jwt_err) => {
                ("INVALID_HEADER", Some(jwt_error_to_reason(jwt_err).into()))
            }
            AuthenticationError::CannotCreateDecodingKey(jwt_err) => (
                "CANNOT_CREATE_DECODING_KEY",
                Some(jwt_error_to_reason(jwt_err).into()),
            ),
            AuthenticationError::JWKHasNoAlgorithm => ("JWK_HAS_NO_ALGORITHM", None),
            AuthenticationError::CannotDecodeJWT(jwt_err) => (
                "CANNOT_DECODE_JWT",
                Some(jwt_error_to_reason(jwt_err).into()),
            ),
            AuthenticationError::CannotInsertClaimsIntoContext(_) => {
                ("CANNOT_INSERT_CLAIMS_INTO_CONTEXT", None)
            }
            AuthenticationError::CannotFindKID(_) => ("CANNOT_FIND_KID", None),
            AuthenticationError::CannotFindSuitableKey(_, _) => ("CANNOT_FIND_SUITABLE_KEY", None),
            AuthenticationError::InvalidIssuer { .. } => ("INVALID_ISSUER", None),
            AuthenticationError::InvalidAudience { .. } => ("INVALID_AUDIENCE", None),
            AuthenticationError::UnsupportedKeyAlgorithm(_) => ("UNSUPPORTED_KEY_ALGORITHM", None),
        };

        ErrorContext {
            message: self.to_string(),
            code: code.into(),
            reason,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ErrorContext {
    pub(super) message: String,
    pub(super) code: String,
    pub(super) reason: Option<String>,
}

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("header_value_prefix must not contain whitespace")]
    BadHeaderValuePrefix,
}
