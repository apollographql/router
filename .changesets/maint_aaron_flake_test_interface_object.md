### Fix arm-Linux flake in `plugins::connectors::tests::test_interface_object`

Use `Plan::Sequence` + `Plan::Parallel` from `req_asserts` instead of positional matching for the five entity-resolution fetches that the connectors plugin issues in parallel. The previous positional assertion happened to pass on most runs because the fetches typically arrived in a stable order, but arm-Linux CI scheduling exposed a legitimate ordering race where `/itfs/2` arrived before `/itfs/1`, producing `"[Request 3]: Expected path /itfs/1, got /itfs/2"`. Only the test asserts changed; production behavior is unaffected.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/0
