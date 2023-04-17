### prevent span attributes from being formatted to write logs 

we do not show span attributes in our logs, but the log formatter still spends some time formatting them to a string, even when there will be no logs written for the trace. This adds the `NullFieldFormatter` that entirely avoids formatting the attributes

By [@Geal](https://github.com/Geal) in https://github.com/apollographql/router/pull/2890