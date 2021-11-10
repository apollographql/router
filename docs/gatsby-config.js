const themeOptions = require("gatsby-theme-apollo-docs/theme-options");

module.exports = {
  plugins: [
    {
      resolve: "gatsby-theme-apollo-docs",
      options: {
        ...themeOptions,
        root: __dirname,
        pathPrefix: "/docs/router",
        subtitle: "Router (pre-alpha)",
        description: "Documentation for the Apollo Router",
        githubRepo: "apollographql/router",
        sidebarCategories: {
          null: ["index", "quickstart", "configuration", "build-from-source"],
        },
      },
    },
  ],
};
