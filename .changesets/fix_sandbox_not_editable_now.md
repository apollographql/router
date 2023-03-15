### Sandbox Explorer endpoint URL no longer editable ([PR #2729](https://github.com/apollographql/router/pull/2729))

The "Endpoint" in the Sandbox Explorer (Which is served by default when running in development mode) is no longer editable, to prevent inadvertent changes.  This was the original intention, particularly since CORS restrictions will generally make it difficult to access another host anyways without configurations being made on that other host.  Since this version of Sandbox is specifically for the version of Router you are developing against, the locked behavior seems to make the most sense.

A hosted version of Sandbox Explorer without this restriction [is still available](https://studio.apollographql.com/sandbox/explorer) if you necessitate a version which allows editing.

By [@mayakoneval](https://github.com/mayakoneval) in https://github.com/apollographql/router/pull/2729
