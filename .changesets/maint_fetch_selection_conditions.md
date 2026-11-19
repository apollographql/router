### Keep fetch-selection conditions consistent after removing inputs

Invalidate cached execution conditions when input selections are removed from a fetch. This hardens the selection mutation API for callers that read conditions before changing the selection.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10288
