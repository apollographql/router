### Place the router and supergraph stages' layers only where the configuration uses them

Every request passed the batch split, persisted queries' expansion and safelist, the request display, the operation limits walk, the fleet detector's request and fetch counters and the operations counter, whatever was configured. Each is now placed only where something uses it:

- the batch split where batching is enabled. Where it is not, a batch is refused as before (`BATCHING_NOT_ENABLED`, `batching not enabled`, 400), now by the layer that reads the request body, which a non-batch request's body now passes through once instead of twice;
- persisted queries' expansion and safelist where a persisted query manifest is configured;
- the request display where the router's `request` event is logged;
- the operation limits where a limit is configured or telemetry reads the measures (usage reporting, spans, or a configured instrument or event);
- the fleet detector's counters and `apollo.router.operations` where a meter provider with a reader admits them: with an Apollo key and graph reference they stay, since Apollo's metrics read them;
- the static page where the sandbox or the homepage is served.

No answer changes. A metrics exporter added by a reload receives these counts from the new pipeline on.

By [@martijnwalraven](https://github.com/martijnwalraven)
