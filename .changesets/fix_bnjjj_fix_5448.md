### Fix events when trace is not sampled, and keep event attribute types ([PR #5464](https://github.com/apollographql/router/pull/5464))

Several fixes about events:

+ Keep original attribute type and not convert it to string (also perf improvement)
+ Use `http.response.body.size`  and `http.request.body.size` as a number and not a string
+ Display custom event attributes even if the trace is not sampled 



> :warning: If you had some monitoring enabled on your logs please be cautious because with these changes, now instead of having for example an attribute like `http.response.status_code = "200"` it will be `http.response.status_code = 200` the 200 code will be a number and not a string anymore. That's an example. Same for `http.request|response.body.size` attribute


By [@bnjjj](https://github.com/bnjjj) in https://github.com/apollographql/router/pull/5464