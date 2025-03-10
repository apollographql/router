### Add apollo.router.pipelines metrics ([PR #6967](https://github.com/apollographql/router/pull/6967))

When the Router reloads either via schema change or config change a new request pipeline is created.
Existing request pipelines are closed once their requests finish. However, this may not happen if there are ongoing long requests 
such as Subscriptions that do not finish.

To enable debugging when request pipelines are being kept around a new gauge metric has been added:

- `apollo.router.pipelines` - The number of request pipelines active in the router
    - `schema.id` - The Apollo Studio schema hash associated with the pipeline.
    - `launch.id` - The Apollo Studio launch id associated with the pipeline (optional).
    - `config.hash` - The hash of the configuration

By [@BrynCooke](https://github.com/BrynCooke) in https://github.com/apollographql/router/pull/6967
