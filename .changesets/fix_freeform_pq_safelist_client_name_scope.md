### Persisted query safelist respects client-name scope in freeform matching

Body-based (freeform) persisted-query safelist matching previously ignored the `clientName` scope declared in the PQ manifest, so an operation registered for one client was accepted for any client. Freeform matching now respects the manifest's client-name scope — trying the request's client name first and falling back to a client-agnostic entry — consistent with how ID-based lookup already behaves. Note that `clientName` is a self-reported, unauthenticated header and is not an authorization boundary.

By [@carodewig](https://github.com/carodewig)
