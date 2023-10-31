### Rename helm template from common\. to apollographql\. ([Issue #4002](https://github.com/apollographql/router/issues/4002))

There is a naming clash with bitnami common templates used in other charts. This is unfortunate when used in a chart which has multiple dependencies where names may clash.

The straightforward fix is to rename our templates from common to apollographql.

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/4005