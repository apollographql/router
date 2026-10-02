### Bound multipart framing in file upload requests ([PR #9959](https://github.com/apollographql/router/pull/9959))

The file uploads plugin bounded the *content* of a `multipart/form-data` request — per-file bytes via `max_file_size`, the GraphQL operation via `http_max_request_bytes`, and the `map` field via an internal cap — but not the framing around it. A `multipart/form-data` body may also carry a preamble before the first boundary, a header block per part, boundary delimiters, and transport padding, and the parser has to buffer the preamble and each header block in full before it can act on them. Neither was subject to any limit, so a single request with an endless preamble, or one part carrying an enormous `Content-Disposition`, could grow router memory until the process was terminated.

A new limit, `preview_file_uploads.protocols.multipart.limits.max_overhead_size`, bounds how much framing a single upload request may contain. It defaults to `2mb`:

```yaml
preview_file_uploads:
  enabled: true
  protocols:
    multipart:
      limits:
        max_overhead_size: 2mb
```

Because the multipart parser cannot distinguish a framing byte from a content byte, the router adds this allowance to the content the other limits already permit and enforces the sum as a limit on the total request; exceeding it returns `413 Payload Too Large`. The total therefore scales with `max_file_size` and `max_files`, as you would expect, and a request that fits within the configured file and operation limits cannot trip it.

One behavior change to note: parts that the `map` field never references were previously skipped without any size limit at all. They now count toward the total, so a request cannot use unreferenced parts to stream unbounded data through the router.

By [@carodewig](https://github.com/carodewig) in https://github.com/apollographql/router/pull/9959
