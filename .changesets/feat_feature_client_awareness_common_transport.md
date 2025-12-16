###  ([PR #8503](https://github.com/apollographql/router/pull/8503))

Supporting common transport mechanisms for the Client Awareness and Enhanced Client Awareness values is more efficient than the current split between HTTP header (for Client Awareness) and `request.extensions` for Enhanced Client Awareness. This changeset allows clients to send the library name/version values as HTTP headers.

By [@calvincestari](https://github.com/calvincestari).
