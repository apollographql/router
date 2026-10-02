### Connector validation no longer panics on `->first` of an empty or non-ASCII string literal

Static shape checking of a connector selection such as `$("")->first` or `$("é")->first` panicked because the inferred result took the first UTF-8 byte rather than the first character. This affected composition and loading a supergraph containing such a selection. The inferred shape now matches runtime: the first character, or no value for an empty string.

By [@inanna-apollo](https://github.com/inanna-apollo) in https://github.com/apollographql/router/pull/10365
