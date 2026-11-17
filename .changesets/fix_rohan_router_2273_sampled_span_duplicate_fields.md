### Stop repeating span attributes in logs from sampled traces ([PR #10754](https://github.com/apollographql/router/pull/10754))

In logs written during a sampled trace, a span attribute that the router set after the span started could appear twice. For example, text logs showed `apollo.connector.source.name` twice in a connector span, and JSON logs could repeat a key inside the `span` object. Each attribute now appears once. Logs from unsampled requests weren't affected.

If your log pipeline worked around the repeated keys, you can remove the workaround.

By [@rohan-b99](https://github.com/rohan-b99) in https://github.com/apollographql/router/pull/10754
