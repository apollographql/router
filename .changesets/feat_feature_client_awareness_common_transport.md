###  ([PR #8503](https://github.com/apollographql/router/pull/8503))

Supporting a common transport mechanism for the Client Awareness and Enhanced Client Awareness values is more efficient than the current split between HTTP header (for Client Awareness) and `request.extensions` for Enhanced Client Awareness. This changeset allows clients to send both sets of values using the same method.

By [@calvincestari](https://github.com/calvincestari).
