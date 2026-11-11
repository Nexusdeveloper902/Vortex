# Releasing Vortex

Releases are automated with release-please. Nobody bumps versions, edits `CHANGELOG.md` or pushes
tags by hand in the normal flow. This file is the whole procedure; if something here is wrong,
fix this file in the same PR that changes the pipeline.

## The flow

1. PRs are squash-merged into `main` with Conventional Commit titles (CLAUDE.md, Workflow). The
   squash title is what release-please reads, so the PR title must be a valid Conventional Commit.
2. Every push to `main` runs `.github/workflows/release.yml`, job `release-please`. It opens or
   updates one release PR titled `chore(main): release X.Y.Z` on branch
   `release-please--branches--main--components--vortex`. The PR bumps `Cargo.toml`
   (`[workspace.package] version`), `.release-please-manifest.json`, `version.txt` and adds the
   `CHANGELOG.md` section.
3. In the same job, the step "Sync Cargo.lock on the release PR" checks out that branch, runs
   `cargo update --workspace` and pushes a `chore: sync Cargo.lock` commit. release-please cannot
   bump workspace crates in `Cargo.lock`, and every build uses `--locked`/`--frozen`.
4. A maintainer (or an agent the owner authorized) merges the release PR. That push makes
   release-please create the GitHub Release and the tag `vX.Y.Z`, with output
   `release_created=true`.
5. The same workflow run then builds and publishes:
   - `tarball`: `archlinux:latest` container, `cargo build --release --locked -p vortex-gtk`,
     `packaging/make-tarball.sh X.Y.Z` → `vortex-X.Y.Z-x86_64-linux.tar.gz`.
   - `deb`: `debian:trixie` container, Rust via rustup (trixie's rustc is older than the MSRV),
     `cargo deb -p vortex-gtk --locked` → `vortex_X.Y.Z-1_amd64.deb`.
   - `rpm`: `fedora:latest` container, `cargo generate-rpm -p crates/vortex-gtk` →
     `vortex-X.Y.Z-1.x86_64.rpm`.
   - `publish`: downloads the three, writes `sha256sums.txt`, creates a build provenance
     attestation (`actions/attest-build-provenance`) and runs `gh release upload --clobber`.

Tags created by release-please with `GITHUB_TOKEN` do not trigger other workflows, which is why the
build jobs hang off `release_created` in the same run and not off a tag push.

## Version rules

- Single source of truth: `[workspace.package] version` in the root `Cargo.toml`. Both crates use
  `version.workspace = true`. `.release-please-manifest.json` and `version.txt` mirror it and are
  only ever changed by release-please.
- Bumps come from commit types since the last release: `fix:` → patch, `feat:` → minor,
  `!` or a `BREAKING CHANGE:` footer → major. `docs:`, `ci:`, `test:`, `chore:` alone do not
  cause a release PR.
- To force a version, put `Release-As: X.Y.Z` in the body of a commit merged to `main`. Do not
  edit the version files by hand.
- One version and one tag (`vX.Y.Z`, no component prefix) for the whole workspace.

## Before merging a release PR (checklist)

1. The PR has both commits: `chore(main): release X.Y.Z` and `chore: sync Cargo.lock`. If the
   second is missing, the latest `Release` workflow run on `main` failed or has not finished.
2. `gh pr diff <n>` shows `Cargo.toml`, `Cargo.lock` (exactly the `vortex-core` and `vortex-gtk`
   `version` lines), `.release-please-manifest.json`, `version.txt`, `CHANGELOG.md`. Nothing else.
3. The lockfile commit was pushed with `GITHUB_TOKEN`, so CI does not run on it. That is expected;
   the release builds use `--locked` and fail loudly on a mismatch.
4. Squash-merge it: `gh pr merge <n> --squash --delete-branch`.
5. Publishing a release is outward-facing: only merge a release PR when the owner asked for that
   release.

## After merging: verify

```sh
id=$(gh run list --workflow release.yml --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$id"
gh release view vX.Y.Z --json assets --jq '.assets[].name'   # tar.gz, .deb, .rpm, sha256sums.txt
gh release download vX.Y.Z -D /tmp/vortex-rel && (cd /tmp/vortex-rel && sha256sum -c sha256sums.txt)
gh attestation verify /tmp/vortex-rel/vortex_*.deb --repo Nexusdeveloper902/Vortex
```

Report the run URL and the asset list. If a build job fails, the GitHub Release already exists
without assets: fix the cause in a normal PR, then re-run the failed jobs of that workflow run
(`gh run rerun "$id" --failed`). The build jobs check out the tag, so a fix merged after the tag is
not in the rerun; if the fix must be in the artifacts, cut a new patch release instead.

Manual fallback, only if the owner asks: pushing a `vX.Y.Z` tag runs the build jobs for that tag
and uploads to the existing release of that name (`gh release upload` needs the release to exist).

## Packages and names

| Asset | Built on | Target | Name / binary |
|---|---|---|---|
| tarball | Arch (latest) | recent glibc, GTK ≥ 4.14, libadwaita ≥ 1.6 | `bin/vortex` |
| .deb | Debian 13 trixie | Debian 13+, Ubuntu 25.04+ | `vortex`, `/usr/bin/vortex` |
| .rpm | Fedora latest | Fedora | `vortex`, `/usr/bin/vortex` |
| PKGBUILD (not published) | user's machine | Arch | `vortex-launcher-git`, `/usr/bin/vortex-launcher` |

- Debian 12 and Ubuntu 24.04 are too old (GTK/libadwaita). Do not lower the deb build image.
- On the AUR, `vortex` is an unrelated app (Nexus Mods), hence the Arch-only rename. Per-game
  `.desktop` entries use `current_exe()`, so they follow whichever binary name is installed.
- umu-launcher is not in Debian or Fedora repos: no Depends/Recommends on it; README says to install
  it separately.
- Package metadata lives in `crates/vortex-gtk/Cargo.toml` (`[package.metadata.deb]`,
  `[package.metadata.generate-rpm]`). New files in `data/` must be added to both asset lists and
  to `packaging/make-tarball.sh` and `packaging/arch/PKGBUILD`.
- Out of scope by owner decision: Flatpak, AppImage, publishing to the AUR.

## Files

- `.github/workflows/release.yml`: the pipeline above. `permissions: {}` at top; each job declares
  its own minimal permissions (`publish` is the only job with `contents: write` plus
  `id-token`/`attestations: write`; `release-please` has `contents` and `pull-requests: write`).
- `release-please-config.json`: one root package `.`, `release-type: simple`,
  `include-component-in-tag: false`, `extra-files` bumps `$.workspace.package.version` in
  `Cargo.toml`.
- `.release-please-manifest.json`, `version.txt`, `CHANGELOG.md`: written by release-please.
- `packaging/make-tarball.sh`: tarball layout.

## Required repository setting

Settings → Actions → General → Workflow permissions: "Allow GitHub Actions to create and approve
pull requests" must be on (default token stays read-only). Without it the `release-please` job
fails with `GitHub Actions is not permitted to create or approve pull requests`. Agents cannot
change this setting; ask the owner.

## Maintaining the pipeline

- `googleapis/release-please-action` is third-party and pinned to a full commit SHA with the
  version in a comment. To update: `gh api repos/googleapis/release-please-action/git/ref/tags/vN.M.K`
  (if `object.type` is `tag`, resolve it with `.../git/tags/<sha>`), replace the SHA and comment.
  `actions/*` are GitHub-owned and stay on major tags.
- Lint workflow changes with `docker run --rm -v "$PWD:/repo" -w /repo rhysd/actionlint:latest`.
- Reproduce a package build locally with the same container and commands as the job
  (`docker run --rm -it -v "$PWD:/src" -w /src debian:trixie bash`, then the job's `run:` lines).

## Known failures and their causes

- `illegal pathing characters in path: crates/vortex-gtk/../../CHANGELOG.md`: release-please does
  not accept `..` in `changelog-path`. Fixed by the single root package.
- `GitHub Actions is not permitted to create or approve pull requests`: the repository setting
  above is off.
- `toml` `extra-files` entries with a jsonpath filter on `Cargo.lock`
  (`$.package[?(@.name=='…')].version`) silently do nothing. That is why the lockfile is synced
  with `cargo update --workspace` instead.
- A release PR without the `chore: sync Cargo.lock` commit, if merged, breaks every `--locked`
  build on `main` (CI, release jobs, PKGBUILD). Fix: a PR running `cargo update --workspace`.
