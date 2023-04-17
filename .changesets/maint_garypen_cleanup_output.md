### Remove redundant `println!()` that breaks json formatted logging ([PR #2923](https://github.com/apollographql/router/pull/2923))

The println!() statement is redundant with the nearby WARN log line and, more importantly, disrupts json logging. e.g.
(log extract)

```
Got error sending request for url (https://engine-staging-report.apollodata.com/api/ingress/traces): connection error: unexpected end of file
{"timestamp":"2023-04-11T06:36:27.986412Z","level":"WARN","message":"attempt: 1, could not transfer: error sending request for url (https://engine-staging-report.apollodata.com/api/ingress/traces): connection error: unexpected end of file"}
```

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/2923