### Fix flaky `test_aws_sig_v4_signing` integration test ([Issue #9728](https://github.com/apollographql/router/issues/9728))

This test drives a real network call to live AWS EC2 (STS `AssumeRole` + SigV4-signed `DescribeInstances`), and the router's default 30s connector timeout was occasionally too tight for that round trip from CI, surfacing as a `GATEWAY_TIMEOUT`. The test fixture now overrides the connector's timeout to 60s. No production code changed.

By [@aaronArinder](https://github.com/aaronArinder) in https://github.com/apollographql/router/pull/9731
