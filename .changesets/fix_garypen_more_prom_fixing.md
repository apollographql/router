### Fix the prometheus descriptions as well as the metrics ([Issue #3491](https://github.com/apollographql/router/issues/3491))

I didn't realise the descriptions on the prometheus stats were significant, so my prefious prometheus fix constrained itself to renaming the actual metrics.

This relaxes the regex pattern to include prom descriptions as well as metrics in the renaming.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3492