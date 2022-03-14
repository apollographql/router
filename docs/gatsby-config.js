const themeOptions = require("gatsby-theme-apollo-docs/theme-options");

module.exports = {
  plugins: [
    {
      resolve: "gatsby-theme-apollo-docs",
      options: {
        ...themeOptions,
        root: __dirname,
        pathPrefix: "/docs/router",
        subtitle: "Router (preview)",
        description: "Documentation for the Apollo Router",
        githubRepo: "apollographql/router",
        algoliaIndexName: 'router',
        algoliaFilters: ['docset:router'],
        sidebarCategories: {
          null: ["index", "quickstart", "configuration", "build-from-source"],
          'Managed Federation': [
            'managed-federation/overview',
            'managed-federation/setup',
            '[Studio features](https://www.apollographql.com/docs/studio/federated-graphs/)',
          ],
          'Development workflow': [
            'development-workflow/build-run-queries',
            'development-workflow/requests',
            '[Apollo Studio Explorer](https://www.apollographql.com/docs/studio/explorer/)',
          ],
          'Third-Party Support': [
            '[Subgraph-compatible libraries](https://www.apollographql.com/docs/federation/v2/other-servers/)',
            '[Subgraph specification](https://www.apollographql.com/docs/federation/v2/federation-spec/)',
          ],
        },
      },
    },
  ],
};
