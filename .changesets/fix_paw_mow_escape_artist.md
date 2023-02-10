### Revisit Open Telemetry integration ([Issue #1812](https://github.com/apollographql/router/issues/1812), [Issue #2359](https://github.com/apollographql/router/issues/2359), [Issue #2338](https://github.com/apollographql/router/issues/2338), [Issue #2113](https://github.com/apollographql/router/issues/2113), [Issue #2113](https://github.com/apollographql/router/issues/2113))

There were several issues with the existing Open Telemetry integration in the router:
* It leaked memory
* Metrics did not work after a schema or config update.
* Telemetry config could not be changed at runtime.
* Logging format changed depending on where the log statement occurred in the code.
* On shutdown the following message frequently occurred: `OpenTelemetry trace error occurred: cannot send span to the batch span processor because the channel is closed`

We have revisited the way we integrate with OpenTelemetry and Tracing to fix the above errors, and to bring our use of these libraries in line with best practice.
In addition, the testing coverage for telemetry had been improved significantly.

For more details of what changed and why take a look at https://github.com/apollographql/router/pull/2358. 

By [@bryncooke](https://github.com/bryncooke) and [@geal](https://github.com/geal) and [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/2358
