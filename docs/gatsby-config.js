const themeOptions = require('gatsby-theme-apollo-docs/theme-options');

module.exports = {
  plugins: [
    {
      resolve: 'gatsby-theme-apollo-docs',
      options: {
        ...themeOptions,
        root: __dirname,
        pathPrefix: '/docs/router',
        subtitle: 'Router',
        description: 'A guide to the Router',
        githubRepo: 'apollographql/router',
        sidebarCategories: {
          null: [
            'index',
            'configuration',
          ],
        },
      },
    },
  ],
};
