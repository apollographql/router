### Add support for readiness/liveness checks ([Issue #3233](https://github.com/apollographql/router/issues/3233))

The router is a complex beast and it's not entirely straightforward to determine if a router is live/ready.

From a kubernetes point of view, a service is:

 - live [if it isn't deadlocked](https://www.linkedin.com/posts/llarsson_betterdevopsonkubernetes-devops-devsecops-activity-7018587202121076736-LRxE)
 - ready if it is able to start accepting traffic

(For more details: https://kubernetes.io/docs/tasks/configure-pod-container/configure-liveness-readiness-startup-probes/)

Our current health check isn't actually doing either of these things. It's returning a payload which indicates if the router is "healthy" or not and it's always returning "UP" (hard-coded). As long as it is serving traffic it appears "healthy".

To address this we need:

1. A mechanism within the router to define whether it is ready or live.
2. A new interface whereby an interested party may determine if the router is ready or live.

Both of these questions are significantly complicated by the fact that the router is architected to maintain connectivity in the face of changing configuration, schemas, etc... Multiple interested parties may be operating against the same router with different states (via different connections) and thus may, in theory, get different answers. However, I'm working on the assumption that, for k8s control purposes, a client, will only be interested in the current (latest) state of the router, so that's what we'll track.

---

1. How do we know if the router is ready or live?

Ready

 - Is in state Running
 - Is `Live`

Live
 - Is not in state Errored
 - Health check enabled and responding

2. Extending our existing health interface

If we add support for query parameters named "ready" and "live" to our existing health endpoint, then we can provide a backwards compatible interface which leverages the existing "/health" endpoint. We support both POST and GET for maximum compatibility.

Sample queries:

```
curl -XPOST "http://localhost:8088/health?ready" OR curl  "http://localhost:8088/health?ready"

curl -XPOST "http://localhost:8088/health?live" OR curl "http://localhost:8088/health?live"
```

To keep the implementation simple, you may only query for one at a time (which I think is fine).

By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3276
