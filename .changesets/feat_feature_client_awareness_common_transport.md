### Support client awareness metadata via HTTP headers ([PR #8503](https://github.com/apollographql/router/pull/8503))

Clients can now send library name and version metadata for client awareness and enhanced client awareness using HTTP headers. This provides a consistent transport mechanism instead of splitting values between headers and `request.extensions`.

By [@calvincestari](https://github.com/calvincestari) in https://github.com/apollographql/router/pull/8503
