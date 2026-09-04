---
name: release
description: Cut a txcript release — preflight the workspace, bump versions, tag, watch the publish-crates and publish-npm workflows, and verify on crates.io and npm. Use when asked to release, publish, or ship a new version of txcript.
argument-hint: [version]
---

# Release txcript

Publishes the `txcript` library to crates.io, the WASM package to npm, and a
GitHub Release with notes, via the tag-triggered `publish-crates`,
`publish-npm`, and `release` workflows. Three releases (v0.1.0–v0.3.0)
established this procedure; follow it in order. A crates.io
version is **permanent** — it can be yanked but never deleted or reused — so
every gate runs before the tag exists.

## Inputs

`$ARGUMENTS`: optional target version. If absent, propose one from the diff
since the last tag: breaking API change on 0.x → minor bump, otherwise patch.
Confirm the version with the user before tagging — this is the one
irreversible decision.

## Preflight (before any version edit)

1. Working tree clean, on `main`, up to date with origin. Uncommitted files
   fail `cargo publish --locked`.
2. `cargo test --workspace` — bare `cargo test` skips the CLI member.
3. `cargo clippy --workspace --all-targets` — pedantic baseline,
   `unwrap/expect/panic` denied in `src/`.
4. `cargo fmt --all --check` — CI gates on this and the other preflight
   commands do not, so unformatted code passes every check here and fails the
   run you're waiting on before tagging (cost a round trip at v0.5.0).
5. `cargo test --no-default-features` — the featureless build is a supported
   surface and has broken independently of the default build before.
6. `cargo publish --dry-run --locked -p txcript` — catches packaging errors
   (missing metadata, dirty files) that the workflow would only surface after
   the tag is pushed.

## Bump and tag

1. Set the new version in **all four** places: `Cargo.toml` (root `txcript`),
   `cli/Cargo.toml` (`txcript-cli` — `publish = false`, but its version tracks
   the library), the `txcript = { version = … }` pin inside `cli/Cargo.toml`,
   and `package.json` (the npm/WASM package). The pin only *has* to move on a
   minor bump — `^0.4.0` admits 0.4.3 but not 0.5.0 — which is exactly when
   it's easiest to forget. `package.json` had drifted three releases behind by
   v0.5.0; the npm workflow guards tag-vs-`package.json`, so drift there is a
   dispatch-time failure, not a tag-time one. `cargo check` once so
   `Cargo.lock` picks up the bump; commit the lockfile with the manifests.
2. Update `CHANGELOG.md`: rename the `## [Unreleased]` section to
   `## [X.Y.Z] - YYYY-MM-DD`, open a fresh empty `## [Unreleased]` above it,
   and add the `[X.Y.Z]: …/compare/v<prev>...vX.Y.Z` link at the bottom
   (repoint `[Unreleased]` at the new tag). Every user-visible change since
   the last tag must be under Added/Changed/Fixed/Removed. The `release`
   workflow takes the GitHub Release notes from this section and **fails the
   tag** when the section is missing, so check it locally first:
   `.github/scripts/release-notes.sh X.Y.Z`.
3. Commit the bump and changelog together, push, and confirm CI is green on
   that commit before tagging.
4. Annotated tag matching the manifest exactly:
   `git tag -a v<X.Y.Z> -m "v<X.Y.Z>" && git push origin v<X.Y.Z>`.
   The workflow's first step compares `${GITHUB_REF_NAME#v}` against the
   manifest and hard-fails on mismatch.

## Watch and verify

1. The tag push fires **three** workflows: `publish-crates` (cargo, with
   `cargo publish --locked -p txcript` — only the library ships),
   `publish-npm` (builds the WASM bundle and publishes via OIDC trusted
   publishing — no token, npm trusts this repo + workflow filename as of
   v0.5.0), and `release` (creates the GitHub Release with the changelog
   section as notes). Watch all three runs (`gh run watch` in the
   background).
2. Verify crates.io: `cargo search txcript` or fetch
   `https://crates.io/api/v1/crates/txcript` and check `max_version`.
3. Verify npm: `npm view txcript version`. If the npm run failed on its
   version guard, `package.json` missed the bump (step 1 of Bump and tag);
   fix the manifest, then re-dispatch on the tag is not possible — the tag
   must carry the right `package.json`, so a failed guard means cutting a
   patch release with the manifest fixed.
4. Verify the release: `gh release view vX.Y.Z` shows the changelog notes and
   the crates.io / npm / docs.rs links.

## Report

State the published version, the three workflow run URLs, the GitHub Release
URL, and the crates.io and npm verification results.
