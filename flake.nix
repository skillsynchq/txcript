{
  description = "txcript — Rust toolchain and native build dependencies";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        # Matches the CI `test` job: stable, plus rustfmt and clippy, plus the
        # wasm32 target the `wasm` job checks against. rust-src is not used by
        # CI but rust-analyzer needs it.
        rust = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rustfmt" "clippy" ];
          targets = [ "wasm32-unknown-unknown" ];
        };

        # The CI `msrv` job pins this exactly. Keep in lockstep with
        # `rust-version` in Cargo.toml when you bump the toolchain.
        msrv = pkgs.rust-bin.stable."1.96.0".default;

        # Native build inputs, and why each one is here:
        #   cmake, perl — wreq builds BoringSSL from source
        #   pkg-config  — standard -sys crate discovery
        #   clang/llvm  — libclang, for any bindgen in the -sys chain
        # rusqlite uses the `bundled` feature and compiles SQLite from C, so it
        # needs a C compiler but no system libsqlite; stdenv supplies that.
        nativeDeps = with pkgs; [
          pkg-config
          cmake
          perl
          clang
        ];

        darwinDeps = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin
          (with pkgs; [ libiconv ]);

        # The npm package build: `wasm-bindgen-cli` MUST match the
        # `wasm-bindgen` crate version in Cargo.lock (currently 0.2.126) or the
        # generated bindings fail to load. If nixpkgs drifts, pin it with an
        # override rather than letting the versions diverge.
        jsDeps = with pkgs; [
          wasm-bindgen-cli
          nodejs
          bun
          # Deploying the Cloudflare Worker in deploy/cloudflare.
          wrangler
        ];

        shellFor = toolchain: extra: pkgs.mkShell {
          packages = [ toolchain ] ++ nativeDeps ++ darwinDeps ++ extra;

          # bindgen, if it appears anywhere in the dependency tree, needs to be
          # told where libclang lives; nixpkgs does not put it on a default
          # search path.
          LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";

          shellHook = ''
            echo "txcript dev shell — $(rustc --version)"
          '';
        };

        # A *minimal* toolchain for building packages: rustc and cargo only.
        #
        # Not the dev-shell toolchain. That one carries rust-docs, clippy,
        # and rustfmt, and building with it drags all three into the
        # binary's runtime closure — 1.9 GiB of it, for a 2 MB executable,
        # which then lands in the container image.
        buildToolchain = pkgs.rust-bin.stable.latest.minimal;

        rustPlatform = pkgs.makeRustPlatform {
          cargo = buildToolchain;
          rustc = buildToolchain;
        };

        share-server = pkgs.callPackage ./nix/package.nix { inherit rustPlatform; };
      in
      {
        packages = {
          default = share-server;
          inherit share-server;

          # The same host with the S3 backend compiled in. Separate because a
          # filesystem deployment should not ship or audit the AWS client.
          share-server-s3 = pkgs.callPackage ./nix/package.nix {
            inherit rustPlatform;
            features = [ "s3" ];
          };

          # Access verification compiled in, so the origin checks the
          # assertion itself rather than trusting the tunnel's header. Two
          # builds because the store is the other axis, and a binary has to
          # contain whichever pair a deployment actually uses.
          share-server-access = pkgs.callPackage ./nix/package.nix {
            inherit rustPlatform;
            features = [ "cloudflare_access" ];
          };

          share-server-s3-access = pkgs.callPackage ./nix/package.nix {
            inherit rustPlatform;
            features = [
              "s3"
              "cloudflare_access"
            ];
          };

          # An OCI image: the binary's closure, no base image, no distro.
          container = pkgs.callPackage ./nix/container.nix { inherit share-server; };
        };

        devShells = {
          # `nix develop` — everything CI needs except the npm packaging step.
          default = shellFor rust [ ];

          # `nix develop .#js` — adds the wasm-bindgen/bun/node toolchain.
          js = shellFor rust jsDeps;

          # `nix develop .#msrv` — reproduces the CI msrv job exactly.
          msrv = shellFor msrv [ ];
        };

        formatter = pkgs.nixpkgs-fmt;
      })
    // {
      # System-independent outputs. The service module and the authentication
      # module are deliberately separate: swapping how callers are identified
      # must not rebuild or reconfigure the service itself.
      nixosModules = {
        default = import ./nix/module.nix { inherit self; };
        txcript-share = import ./nix/module.nix { inherit self; };
        cloudflare-access = import ./nix/cloudflare-access.nix { inherit self; };
      };
    };
}
