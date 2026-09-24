# Cloudflare Access in front of the service, as a separate import.
#
# This is the whole point of the split: the service package knows nothing
# about Cloudflare, and changing the authentication story means importing a
# different module rather than rebuilding or reconfiguring the service. An
# OIDC or mTLS module would sit here as a peer.
{ self }:
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.txcript-share.cloudflareAccess;
  packages = self.packages.${pkgs.stdenv.hostPlatform.system};
  # The store is chosen on the service, the identity here, and the binary has
  # to contain both. Picking the matching build keeps that from being a
  # configuration the deployment can silently get wrong.
  usesS3 = (config.services.txcript-share.store.kind or "") == "s3";
in
{
  # `identityHeader` named a header the service trusted. It no longer trusts
  # one, so the option is gone rather than quietly ignored — a header name
  # left in a configuration would read as "still enforced".
  imports = [
    (lib.mkRemovedOptionModule
      [ "services" "txcript-share" "cloudflareAccess" "identityHeader" ]
      ''
        The service verifies the Access assertion itself now; set `team` and
        `audFile` instead.

        Ownership moves with it: the principal id was a hash of the header
        value and is now a hash of the Access identity, so transcripts
        published under the old scheme keep their old owner prefix and are
        no longer writable by the person who published them. Re-publish them
        or migrate the keys before switching a store that has content in it.
      ''
    )
  ];

  options.services.txcript-share.cloudflareAccess = {
    enable = lib.mkEnableOption "Cloudflare Access in front of txcript-share";

    team = lib.mkOption {
      type = lib.types.str;
      example = "example";
      description = ''
        The Zero Trust team: the `<team>` of `<team>.cloudflareaccess.com`.
        Its published key set is what assertions are verified against.
      '';
    };

    audFile = lib.mkOption {
      type = lib.types.path;
      description = ''
        A file holding the Access application's AUD tag, from sops/agenix.

        A path rather than a value, like every other credential here, and
        loaded through systemd credentials. A token for some *other*
        application in the same team is signed by the same keys, so this tag
        is what stops it authenticating here.
      '';
    };

    tunnelCredentialsFile = lib.mkOption {
      type = lib.types.path;
      description = "cloudflared tunnel credentials, from sops/agenix.";
    };
  };

  config = lib.mkIf (cfg.enable && config.services.txcript-share.enable) {
    # Selecting the identity source is all this module does to the service.
    #
    # The service verifies the assertion itself, so the tunnel is the ingress
    # and no longer the thing holding identity up: a request that reaches the
    # origin by another route carries no signature the service will accept.
    services.txcript-share.identity = {
      kind = "cloudflare_access";
      team = cfg.team;
      # Where systemd exposes the credential below. Named rather than
      # interpolated because the configuration file is generated at build
      # time, when $CREDENTIALS_DIRECTORY does not exist yet.
      aud_file = "/run/credentials/txcript-share.service/access-aud";
    };
    services.txcript-share.credentialFiles.access-aud = cfg.audFile;

    # Verification is a compiled-in feature, so the default package is the
    # one that has it. Overridable, as long as the replacement also does.
    services.txcript-share.package = lib.mkDefault (
      if usesS3 then packages.share-server-s3-access else packages.share-server-access
    );

    # Loopback by default all the same. Verification means a directly
    # reachable origin is no longer a forgery hole, but there is still no
    # reason to offer one.
    services.txcript-share.listenAddress = lib.mkDefault "127.0.0.1";

    systemd.services.cloudflared-txcript-share = {
      description = "cloudflared tunnel for txcript-share";
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];
      serviceConfig = {
        ExecStart = "${pkgs.cloudflared}/bin/cloudflared tunnel --no-autoupdate run --credentials-file \${CREDENTIALS_DIRECTORY}/tunnel";
        LoadCredential = [ "tunnel:${cfg.tunnelCredentialsFile}" ];
        DynamicUser = true;
        Restart = "on-failure";
        RestartSec = "5s";
      };
    };
  };
}
