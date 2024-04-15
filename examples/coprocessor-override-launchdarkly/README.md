# Progressive Override + LaunchDarkly Router Coprocessor

This Node.js App is a router coprocessor for the progressive `@override` feature in combination with LaunchDarkly. Feature flag rollout information is pulled from LaunchDarkly and is used to set override labels on the request context. These override labels are then provided as input to the query planner to enable progressive migration of fields from one subgraph to another.

## Usage

1. Configure LaunchDarkly environment variables in `.env` file
2. Install and compile: `npm install`
3. Run: `npm start`
4. Update your `router.yaml` to include your coprocessor. See `router.example.yaml` in this directory for an example and what the coprocessor requires.