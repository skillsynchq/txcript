---
name: release
description: Cut a txcript release — dispatch the prepare-release workflow, approve the plan, watch the tag publish to crates.io, npm, and GitHub Releases, and verify. Use when asked to release, publish, or ship a new version of txcript.
argument-hint: [version]
---

# Release txcript

Releases are cut by two workflows. `prepare-release` computes the version
from the conventional-commit PR titles merged since the last tag, generates
the `CHANGELOG.md` section, bumps every manifest, and lands one commit plus
its tag on main. The tag runs `release`, which publishes crates.io, npm, and
CLI binaries, then creates the GitHub Release from the changelog section.

Nothing runs locally. Main is releasable by construction: `ci` proves the
crate packages, the npm bundle builds, and the CLI builds in release mode on
every PR, and the ruleset requires PRs to be up to date before merge.

## Inputs

`$ARGUMENTS`: optional version. Without one, the workflow derives it: a
`feat` PR since the last tag is a minor bump, otherwise patch, and a breaking
change stays a minor bump until 1.0. Preview locally with
`git-cliff --bumped-version` and `git-cliff --unreleased --tag vX.Y.Z --strip all`.

## Procedure

1. Confirm main's latest CI run is green: `gh run list --branch main -L 3`.
2. Dispatch: `gh workflow run prepare-release.yml` (add `-f version=X.Y.Z`
   to override). Then `gh run watch` the run.
3. The Plan job writes the version and changelog section to the run summary
   and the Tag job waits on the `release` environment. Read the summary,
   then approve in the Actions UI. This is the one irreversible decision: a
   crates.io version can be yanked but never reused.
4. The Tag job pushes `chore(release): vX.Y.Z` and the tag atomically. If
   main moved mid-run it rebuilds the commit on the new head; if the derived
   version changed because of what landed, it fails and asks for a rerun.
5. The tag starts `release`. Watch it: `gh run list --workflow release.yml -L 1`
   then `gh run watch <id>`. Jobs: verify, crates.io, npm, binaries for five
   targets, GitHub Release.

## Verify

- crates.io: `cargo search txcript` or `https://crates.io/api/v1/crates/txcript`.
- npm: `npm view txcript version`.
- Release: `gh release view vX.Y.Z` shows the changelog notes, the package
  links, and ten assets (five archives, five checksums).
- `cargo binstall --git https://github.com/skillsynchq/txcript txcript-cli`
  installs the new binary.

## When something fails

- `verify` fails on a manifest mismatch: the tag was pushed by hand without
  a bump. Delete nothing; run `prepare-release` for the next patch.
- A publish job fails after the tag exists: fix forward. The crates.io
  version may already be taken, so the next run needs a new version.
- `release` can be rerun on an existing tag with
  `gh workflow run release.yml --ref vX.Y.Z`; the GitHub Release step fails
  if the release already exists, which is the intended guard.

## Report

State the version, the prepare and release run URLs, the release URL, and
the crates.io and npm verification results.
