## 🗺️🔭 Roadmap

We'll be working to open issues and surface designs about these things and more, as the planning progresses.  Follow this file for more details as they become available and we'll link the items below to issues as we create them.  We look forward to your participation in those discussions!

- **Apollo Studio integrations**

  We'll be building out stories for Apollo Studio, including:
  
  - Tracing
  - Metrics reporting
  - Schema reporting
  
  We'd like to make the Apollo Router as much as part of the Studio story as Apollo Gateway is today.
  
- **Newer Federation 2 features**

  As new Apollo Federation 2 features are released, we'll integrate those updates into the Router.  In most cases, this will be just as simple as updating the Apollo Router's dependencies.

- **More customizations**

  We're excited about a number of opportunities for customizing behavior, including exploring options for:
  
   - Header manipulation
   - Request context propagation
   - Dynamic routing
   - Authorization
   - Auditing
   
  We hope to provide first-class experiences for many of the things which required a more-than-ideal amount of configuration.
  
- **Specification compliance**

  We're still working on making the Apollo Router fully GraphQL specification compliant.  This will be a continued effort and is also embodied in the Apollo Router's design principles.
  
  Until we finish this work, there may be responses returned to the clients which are not fully specification compliant, including artifacts of Federation query plan execution. (e.g., the inclusion of additional metadata).
  
- **OpenTelemetry/Prometheus metrics**

  These will compliment the existing OpenTelemetry traces which we already support. It will help to paint a clearer picture of how the Apollo Router is performing and allow you to set alerts in your favorite alerting software.

- **Structured logging**

  The logs that are produced from the Apollo Router should integrate well with existing log facilities.  We'll be adding configuration to enable this (e.g., JSON-formatted logging).

- **Continued performance tuning**

  The Apollo Router is already fast, but we'll be looking for more ways to make it faster.  We'll be setting up CI/CD performance measurements to track regressions before they end up in user's deployments and to understand the cost of new features we introduce.

- **Hardening**

  The Router will need new functionality to remain performant.  This will include exploring options for rate-limiting, payload size checking, reacting to back-pressure, etc.
