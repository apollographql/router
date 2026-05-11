### fix(file_uploads): apply `http_max_request_bytes` only to the operations field, not file streams ([PR #9226](https://github.com/apollographql/router/pull/9226), [PR #9327](https://github.com/apollographql/router/pull/9327))

Previously, `limits.http_max_request_bytes` (default 2 MB) was applied to the entire multipart body of file upload requests, causing large file uploads to be rejected even when `preview_file_uploads.protocols.multipart.limits.max_file_size` was configured to allow them.

The limit now applies only to the GraphQL operations field (the query and variables). File data is bounded separately by `max_file_size`, enforced by the multer parser.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9226 and https://github.com/apollographql/router/pull/9327
