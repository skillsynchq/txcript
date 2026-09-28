# Cloudflare Access in front of the share service

Access is the **gate**: it stands in front of the service, authenticates
people against your identity provider, and refuses everyone else. The
service verifies the assertion Access attaches — that is the **lock**. You
want both. A gate alone holds only while nothing can walk around it; a
request that reaches the origin some other way carries no signature this
service will accept.

This is setup end to end: the Cloudflare side, the service side, how to
check it actually works, and what the failures look like.

> Transcripts carry `cwd` paths, branch names, source, and tool output.
> Getting this wrong publishes them. Step 7 is not optional.

---

## What you need first

- A Cloudflare Zero Trust team, with an identity provider configured.
- A hostname for the service, on a domain in your Cloudflare account.
- A way for requests to reach the origin — a `cloudflared` tunnel below,
  though any route works now that the origin verifies for itself.
- The service **built with `--features cloudflare_access`**. The default
  build has no verifier in it, and says so at startup rather than guessing:

  ```
  txcript-share-server: parsing configuration: TOML parse error at line 2, column 8
    |
  2 | kind = "cloudflare_access"
    |        ^^^^^^^^^^^^^^^^^^^
  unknown variant `cloudflare_access`, expected `static_tokens` or `forwarded_header`
  ```

  ```sh
  nix build .#share-server-access        # or .#share-server-s3-access
  # or: cargo build -p txcript-share-server --features cloudflare_access --release
  ```

## 1. Your team name

Zero Trust dashboard → **Settings → Custom Pages**, or anywhere the team
domain is shown: it reads `<team>.cloudflareaccess.com`. The `<team>` part
is what the service wants — `example`, not the full domain, and not a URL.
It becomes the host it fetches signing keys from, so it is validated:

```
txcript-share-server: loading credentials: `https://example.cloudflareaccess.com` is not a Zero Trust team name
```

## 2. An Access application, and its AUD tag

Zero Trust → **Access → Applications → Add an application → Self-hosted**,
with the hostname you chose for the service.

Once it exists, its overview shows an **Application Audience (AUD) Tag** — a
64-character hex string. Copy it. Every application in your team is signed
by the *same* keys, so the AUD tag is the only thing that stops a token
minted for one of your other applications from authenticating here. Treat it
as configuration, deliver it like a secret.

## 3. Who gets in

Add policies on the application:

- **People.** An Allow policy with whatever rule you want — emails, a group,
  a domain. These log in through your IdP, and the service sees them by
  their `email`.
- **Machines.** Zero Trust → **Access → Service Auth → Service Tokens →
  Create**, which gives you a Client ID (ending `.access`) and a Client
  Secret, shown once. Then add a second policy on the application with
  action **Service Auth** that includes that token. Without a Service Auth
  policy the token is bounced to the login page like any browser would be.

The service sees a service token by its `common_name`, never an `email`, and
that is deliberate: a robot and a person get **different principals**, so a
CI job cannot overwrite what a human published.

## 4. A route to the origin

The service binds loopback by default. `cloudflared` runs alongside it and
brings requests in through Access:

```sh
cloudflared tunnel login
cloudflared tunnel create txcript-share
cloudflared tunnel route dns txcript-share share.example.com
```

That leaves a credentials file, which is what the NixOS module below wants.
**Ingress rules are the tunnel's own configuration, not this service's** —
point the tunnel at `http://127.0.0.1:8787` in the dashboard or in
`cloudflared`'s config file. Nothing here generates them.

## 5. Configure the service

```toml
listen = "127.0.0.1:8787"

[identity]
kind = "cloudflare_access"
team = "example"
aud_file = "/run/credentials/txcript-share.service/access-aud"

[store]
kind = "filesystem"
root = "/var/lib/txcript-share"

[policy]
kind = "owner_prefix"
```

`aud_file` is a **path**, read once at startup, so systemd `LoadCredential`,
Kubernetes projected secrets, sops, and agenix all work without the binary
knowing any of them exist. Whitespace is trimmed; an empty file is refused
rather than matching nothing:

```
txcript-share-server: loading credentials: the Access AUD file is empty
```

The origin needs egress to `https://<team>.cloudflareaccess.com` for the
signing keys. Roots are compiled in, so no CA bundle is required, but DNS
and outbound 443 are. Keep the clock synced: token expiry is checked with no
leeway, and a host running fast rejects tokens that are still valid.

### On NixOS

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

That is the whole of it: the module writes the identity block, delivers the
AUD tag through systemd credentials, runs `cloudflared` as its own unit, and
selects the Access-capable build — including the S3 one if the store is S3.
If you pin `services.txcript-share.package` yourself, evaluation fails with
an explanation rather than the service restart-looping after the deploy; set
`cloudflareAccess.packageVerifiesAccess` if your own build has the feature.

Swapping Access for something else is an import, not a rebuild of the
service. That is what the separate module is for.

## 6. Point a client at it

```sh
export TXCRIPT_SHARE_URL=https://share.example.com
export TXCRIPT_SHARE_HEADER_ID="CF-Access-Client-Id: ....access"
export TXCRIPT_SHARE_HEADER_SECRET="CF-Access-Client-Secret: ..."
```

Credentials are just a header map, so an Access service token needs no code.
See [Sharing transcripts](share.md) for the client side.

## 7. Verify it against the tenant

Local tests mint real tokens against a key set of their own. What they
cannot cover is Cloudflare, so do this once per deployment:

1. **An unauthenticated browser request gets a 302 to the SSO login**, not a
   401. That is the gate working. A 401 means requests are arriving without
   going through Access.
2. **A human logs in and publishes.** `txcript push <id>` succeeds, and the
   owner segment of the returned slug is the same on a second login.
3. **A service token authenticates** with the two headers above and lands on
   a **different** owner segment from the human. Same segment for both means
   the `common_name` path is not wired up.
4. **A forged assertion is refused.** From a host that can reach the origin
   directly, send a request with a `Cf-Access-Jwt-Assertion` header you made
   up: it must be refused. This is the check that distinguishes verifying
   from trusting, and the reason `forwarded_header` was replaced.
5. **Restart the service and read something.** That exercises the key fetch
   over the real endpoint from inside the unit's sandbox — egress, DNS and
   all.

## Troubleshooting

| What you see | What it means | What to do |
|---|---|---|
| `503` + log `Access key set: fetching https://<team>.cloudflareaccess.com/…: http status: 404` | The team name is wrong. | Fix `team`; it is the label, not the domain. |
| `503` + log `Access key set: fetching …: …timed out` / DNS error | The origin cannot reach Cloudflare. | Open outbound 443 and DNS. It retries every 5s and recovers on its own. |
| `503` for every request, no log line | No key set has ever been fetched *and* nothing is retrying — check the unit started at all. | `systemctl status txcript-share`. |
| Restart loop, `unknown variant cloudflare_access` | The binary has no verifier compiled in. | Build with `--features cloudflare_access`. |
| `loading credentials: the Access AUD file is empty` | The credential did not arrive. | Check `LoadCredential` and the file's contents. |
| Everyone gets `401`, browser login works | The AUD tag belongs to a different application, or the token is for another one. | Copy the AUD from *this* application's overview. |
| Everyone gets `401` right after a clock jump | Expiry is checked with no leeway. | Sync the clock. |
| A service token gets the login page | The application has no Service Auth policy. | Add one that includes the token. |
| Humans work, service tokens `401` | Same as above, or the token was revoked. | Check the policy, then re-issue. |
| Your transcripts vanished from `--owner me` after a switch | Ownership is derived from the Access identity now. | See below. |

## Switching from `forwarded_header`

The principal id used to be derived from the header value and is now derived
from the Access identity, so **ownership moves**. Transcripts published
under the old scheme keep their old owner prefix: still readable by
everyone, no longer writable by the person who published them, and no longer
listed by `--owner me`.

Either start on an empty prefix, or copy the objects across from the old
owner segment to the new one before switching a store that already has
content in it. The NixOS module says as much if you leave the removed
`identityHeader` option in place, rather than letting the change happen
quietly.

## What the service does with the assertion

Briefly, because the failure modes above follow from it:

- The header is `Cf-Access-Jwt-Assertion`, verified as RS256 against the
  team's published keys, with `aud`, `exp` and `iss` all checked.
- The principal id is the SHA-256 of the Access identity — the same digest
  the Cloudflare Worker computes, so both hosts agree on who owns what when
  they front one bucket.
- Keys are fetched by a refresher thread, never on a request: at startup,
  hourly, every 5s while fetching is failing, and whenever a request reports
  a key id the service does not hold. That last one is throttled to a fetch
  a minute, so a forged header cannot turn into traffic aimed at Cloudflare.
- An unrecognised key id against a current key set is a **401**. A key set
  that is missing or out of date is a **503** — "I could not check" is not
  the same answer as "you are not who you say", and only one of them should
  page someone.

`share-server/README.md` has the reference detail.
