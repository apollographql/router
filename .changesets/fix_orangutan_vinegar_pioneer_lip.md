### fix: Traffic shaping configuration fallback for experimental_enable_http2 

Fix a bug where experimental_enable_http2 wouldn't properly apply when a global configuration was set.

Huge thanks to @westhechiang, @leggomuhgreggo @vecchp and @davidvasandani for discovering the issue and finding a reproducible testcase!

By [@o0Ignition0o](https://github.com/o0Ignition0o) in https://github.com/apollographql/router/pull/2976
