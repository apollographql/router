### Accept `@defer` labels reused via fragment spreads

The duplicate `@defer(label:)` check from the [GHSA-gr6h-4wpf-xp52](https://github.com/apollographql/router/security/advisories/GHSA-gr6h-4wpf-xp52) fix ran after fragment expansion, rejecting valid operations where a fragment containing a labeled `@defer` is spread more than once. The incremental delivery specification defines label uniqueness over the document as written, where such a label occurs only once; clients like Relay derive defer labels from fragment names and rely on this.

Label uniqueness is now validated on the document, before fragment expansion, still rejecting the operations behind the original security advisory. Operations that reuse a labeled `@defer` via fragment spreads plan and execute correctly: one incremental response per spread position, each carrying the user-provided label and distinguished by its `path`.

By [@tninesling](https://github.com/tninesling)
