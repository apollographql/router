### Deterministic Builds using Nix

This feature adds deterministic builds and developer environments using [Nix](https://nixos.org/).
Nix allows for standardizing the build and development environments declaratively and deterministically,
allowing for new developers to onboard much faster and for a more consistent developer experience.

To drop into a development shell, make sure that nix is installed and run the following in the root
of the project directory:

```shell
$ nix develop
```

To build the router without needing to download the source code, run the following:

```shell
$ nix build github#apollographql/router
```

By [@nicholascioli](https://github.com/nicholascioli) in https://github.com/apollographql/router/pull/4536
