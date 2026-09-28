# Packaging

Nix builds artifacts. Terraform (or OpenTofu, or Pulumi) provisions
infrastructure and *consumes* an artifact reference — an image tag, an AMI
id. Keeping those two jobs apart is why nothing here reaches out to a cloud
API, and why nothing in a Terraform module needs to know how to compile Rust.

| file | produces |
|---|---|
| `package.nix` | the service binary — every other artifact is derived from it |
| `container.nix` | an OCI image: the binary's closure, no base image, no distro |
| `module.nix` | the NixOS systemd service |
| `cloudflare-access.nix` | `cloudflared` + Access, as a *separate* import |

```sh
nix build .#share-server            # the binary
nix build .#share-server-s3         # with the S3 backend compiled in
nix build .#share-server-access     # with Access verification compiled in
nix build .#share-server-s3-access  # both
nix build .#container               # OCI image, loadable with `docker load -i`
```

## Why the build toolchain is minimal

`package.nix` is built with `rust-bin.stable.latest.minimal`, not the
dev-shell toolchain. The dev shell carries `rust-docs`, `clippy`, `rustfmt`
and `rust-src`; building against it put all of them in the binary's
**runtime** closure — 1.9 GiB, and a 475 MB container image for a 2.2 MB
executable. With the minimal toolchain the closure is 59 MiB and the image is
17 MB.

Most of what remains is `gcc-lib` (56 MiB) for the dynamic libstdc++/libgcc.
Building against `pkgsStatic` with musl would cut that too, at the cost of a
longer build; worth doing if image size starts to matter.

## Auth is a separate module on purpose

`module.nix` knows nothing about Cloudflare. `cloudflare-access.nix` sets the
identity source, runs the tunnel, and selects the build that can verify an
Access assertion. Switching to OIDC or mTLS means importing a different
module — `module.nix` itself is unchanged.

```nix
{
  imports = [
    txcript.nixosModules.txcript-share
    txcript.nixosModules.cloudflare-access
  ];

  services.txcript-share = {
    enable = true;
    store = { kind = "filesystem"; root = "/var/lib/txcript-share"; };
    policy.kind = "owner_prefix";
  };

  services.txcript-share.cloudflareAccess = {
    enable = true;
    team = "example";
    audFile = config.age.secrets.access-aud.path;
    tunnelCredentialsFile = config.age.secrets.tunnel.path;
  };
}
```

Secrets reach the service through systemd `LoadCredential` as **file paths**,
never environment values, which is what lets sops, agenix, and plain files
all work without the binary knowing the difference.

The service verifies the Access assertion itself, against the team's JWKS and
`audFile`. The tunnel is the ingress; it is not what holds identity up, so an
origin someone reaches another way is not a forgery hole. The module also
picks the build that has the verifier compiled in, and fails evaluation
rather than letting a pinned package restart-loop after the deploy.

Setting up the Cloudflare side — team, application, AUD tag, service tokens,
and the checks to run once against a real tenant — is
[docs/cloudflare-access.md](../docs/cloudflare-access.md).

## Machine images

`nixos-generators` turns the module into an AMI, QCOW2, or ISO without
anything extra here — it consumes a NixOS configuration that imports
`module.nix`:

```sh
nix run github:nix-community/nixos-generators -- \
  -f amazon -c ./your-host.nix
```

Not wired as a flake output because the format and the host configuration are
deployment choices, not properties of this repo.
