### Update the planner with a new schema ([Issue #2690](https://github.com/apollographql/router/issues/2690))

Previously, the router was creating a new JS runtime for the planner everytime there's a new schema, and creating one for every API schema creation, and every introspection query call. Creating these runtimes leaks memory, so this change makes sure we keep the same JS runtime for the entire life of the router, and use it across planner updates, API schema generation and introspection queries.

By [@geal](https://github.com/geal) in https://github.com/apollographql/router/pull/2706