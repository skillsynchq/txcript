# The service binary. Every other artifact is derived from this one: a
# container is this plus metadata, a machine image is this plus init, a
# systemd unit is this plus a unit file.
{ lib, rustPlatform, stdenv, features ? [ ] }:

rustPlatform.buildRustPackage {
  pname = "txcript-share-server";
  version = "0.1.0";

  src = lib.cleanSourceWith {
    src = ../.;
    # Keep the store path stable across edits to things the build ignores.
    filter =
      path: type:
      let
        name = baseNameOf path;
      in
      !(builtins.elem name [
        "target"
        "result"
        "node_modules"
        "build"
        ".git"
      ]);
  };

  cargoLock.lockFile = ../Cargo.lock;

  # Only the host binary. The workspace also contains the txcript library,
  # whose SQLite and browser-HTTP dependencies this service has no use for.
  cargoBuildFlags = [
    "-p"
    "txcript-share-server"
  ];
  buildFeatures = features;

  # The test suite runs in CI and in the dev shell, the latter of which can
  # reach a local Silo for the S3 conformance cases. Repeating it inside
  # every package build buys nothing and costs minutes.
  doCheck = false;

  meta = {
    description = "Native HTTP host for txcript's shared transcript service";
    mainProgram = "txcript-share-server";
    platforms = lib.platforms.unix;
  };
}
