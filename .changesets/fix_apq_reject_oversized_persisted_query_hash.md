### Reject oversized `sha256Hash` values before hex-decoding in APQ ([PR #392](https://github.com/apollographql/router-private/pull/392))

The Automatic Persisted Queries (APQ) layer decoded the client-supplied `sha256Hash` extension value with `hex::decode` before checking its length. A valid SHA-256 hash is always exactly 64 hex characters; a request with an arbitrarily large `sha256Hash` string (megabytes of valid hex) would still be hex-decoded in full, allocating memory and burning CPU proportional to the attacker-controlled input size before any comparison against the actual query hash occurred.

The router now rejects any `sha256Hash` whose length is not exactly 64 characters immediately, before attempting to decode it. This matches existing behavior for other malformed hashes: the request falls through to the normal `PERSISTED_QUERY_NOT_FOUND` response.

By [@carodewig](https://github.com/carodewig)
