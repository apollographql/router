{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/release-23.11";
    rust-overlay.url = "github:oxalica/rust-overlay";
    utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    self,
    nixpkgs,
    utils,
    rust-overlay,
  }:
    utils.lib.eachDefaultSystem (system: let
      overlays = [(import rust-overlay)];
      pkgs = import nixpkgs {inherit system overlays;};

      # Set up the custom tooling based on the rust-toolchain.toml file
      toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      rustPlatform = pkgs.makeRustPlatform {
        cargo = toolchain;
        rustc = toolchain;
      };

      commonDeps = with pkgs; [
        protobuf
      ];
      darwinDeps = with pkgs;
        lib.optionals stdenv.isDarwin (with darwin.apple_sdk.frameworks; [
          # TODO: Why are the following frameworks required when we aren't hooking into
          # MacOS internals?
          Security
          SystemConfiguration
        ]);
    in {
      # Expose the main router as a buildable package
      packages.default = rustPlatform.buildRustPackage {
        pname = "router";
        version = "1.38.0";

        src = ./.;
        cargoLock.lockFile = ./Cargo.lock;

        # libz-ng-sys requires cmake for its build script
        # Note: Common deps are needed as native build inputs here because they are needed during
        #   the dependency resolution build step.
        nativeBuildInputs = with pkgs; [ cmake ] ++ commonDeps;
        buildInputs = darwinDeps;

        # The v8 package will try to download a `librusty_v8.a` release at build time to our read-only filesystem
        # To avoid this we pre-download the file and export it via RUSTY_V8_ARCHIVE
        RUSTY_V8_ARCHIVE = import ./nix/librusty_v8.nix { inherit pkgs; };

        # TODO: This should not be disabled, but some tests make network requests and other stateful
        #   operations which won't work in the read-only / sandboxed build env.
        doCheck = false;

        meta = with pkgs.lib; {
          description = "A configurable, high-performance routing runtime for Apollo Federation";
          homepage = "https://github.com/apollographql/router";
          license = licenses.elastic20;
          maintainers = [];
        };
      };

      # Allow for quickly entering a development environment for this project
      devShell = with pkgs;
        mkShell {
          buildInputs = [cargo-insta protobuf toolchain] ++ commonDeps ++ darwinDeps;

          # Let rust-analyzer know where the src (and macro server) is for our
          # version of the toolchain, specified in the `rust-toolchain.toml` file.
          RUST_SRC_PATH = "${toolchain}/lib/rustlib/src/rust";
        };
    });
}
