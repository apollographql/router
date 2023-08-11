### Require the main (GraphQL) route to shutdown before other routes ([Issue #3521](https://github.com/apollographql/router/issues/3521))

This changes router execution so that there is more control over the sequencing of server shutdown. In particular, this modifies how different routes are shutdown so that the main (GraphQL) route is shutdown before other routes are shutdown. Prior to this change all routes shut down in parallel and this would mean that, for example, health checks stopped responding prematurely.
    
This is particularly undesirable when the router is executing in Kubernetes, since continuing to report live/ready checks during shutdown is a requirement.
    
By [@garypen](https://github.com/garypen) in https://github.com/apollographql/router/pull/3557