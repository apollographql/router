### chore: Update rhai to latest release (1.19.0) ([PR #5655](https://github.com/apollographql/router/pull/5655))

In Rhai 1.18.0, there were changes to how exceptions within functions were created. For details see: https://github.com/rhaiscript/rhai/blob/7e0ac9d3f4da9c892ed35a211f67553a0b451218/CHANGELOG.md?plain=1#L12

We've modified how we handle errors raised by Rhai to comply with this change, which means error message output is affected. The change means that errors in functions will no longer document which function the error occurred in, for example:

```diff
-         "rhai execution error: 'Runtime error: I have raised an error (line 223, position 5)\nin call to function 'process_subgraph_response_string''"
+         "rhai execution error: 'Runtime error: I have raised an error (line 223, position 5)'"
```

Making this change allows us to keep up with the latest version (1.19.0) of Rhai.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/5655
