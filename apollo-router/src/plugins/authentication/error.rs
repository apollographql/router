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
        // ErrorKind is non-exhaustive
        _ => "UNKNOWN_ERROR",
    }
}

impl AuthenticationError {
    /// A stable, machine-readable code for this failure. Unlike [`Self::to_string`], this never
    /// contains details of the token or of the router's configuration, so it is safe to report
    /// even when the client-facing message is redacted.
    pub(super) fn code(&self) -> &'static str {
        match self {
            AuthenticationError::CannotConvertToString => "CANNOT_CONVERT_TO_STRING",
            AuthenticationError::InvalidJWTPrefix(_, _) => "INVALID_PREFIX",
            AuthenticationError::MissingJWTToken(_, _) => "MISSING_JWT",
            AuthenticationError::InvalidHeader(_, _) => "INVALID_HEADER",
            AuthenticationError::CannotCreateDecodingKey(_) => "CANNOT_CREATE_DECODING_KEY",
            AuthenticationError::JWKHasNoAlgorithm => "JWK_HAS_NO_ALGORITHM",
            AuthenticationError::CannotDecodeJWT(_) => "CANNOT_DECODE_JWT",
            AuthenticationError::CannotInsertClaimsIntoContext(_) => {
                "CANNOT_INSERT_CLAIMS_INTO_CONTEXT"
            }
            AuthenticationError::CannotFindKID(_) => "CANNOT_FIND_KID",
            AuthenticationError::CannotFindSuitableKey(_, _) => "CANNOT_FIND_SUITABLE_KEY",
            AuthenticationError::InvalidIssuer { .. } => "INVALID_ISSUER",
            AuthenticationError::InvalidAudience { .. } => "INVALID_AUDIENCE",
            AuthenticationError::UnsupportedKeyAlgorithm(_) => "UNSUPPORTED_KEY_ALGORITHM",
        }
    }

    /// The underlying `jsonwebtoken` failure kind, for the variants that wrap one.
    fn reason(&self) -> Option<String> {
        match self {
            AuthenticationError::InvalidHeader(_, jwt_err)
            | AuthenticationError::CannotCreateDecodingKey(jwt_err)
            | AuthenticationError::CannotDecodeJWT(jwt_err) => {
                Some(jwt_error_to_reason(jwt_err).into())
            }
            _ => None,
        }
    }

    pub(super) fn as_context_object(&self) -> ErrorContext {
        ErrorContext {
            message: self.to_string(),
            code: self.code().into(),
            reason: self.reason(),
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
