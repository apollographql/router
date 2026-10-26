### Lower the CPU cost of tracing spans when requests move between them

Since 3.0.0-alpha.2, the router took an extra lock every time a request entered or left one of its internal tracing spans. It no longer does, which lowers CPU time per request. This brings span handling back in line with 2.x.

You don't need to change any configuration.

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/10670
