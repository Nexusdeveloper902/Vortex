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
   release-please create the GitHub Release **as a draft** plus the tag `vX.Y.Z`
   (`"draft": true`, `"force-tag-creation": true`; without the latter GitHub creates no tag for a
   draft, the build jobs could not check it out and release-please would not find the previous
   release next time), with output `release_created=true`. A draft is invisible to the public.
5. The same workflow run then builds and publishes:
   - `tarball`: `archlinux:latest` container, `cargo build --release --locked -p vortex-gtk`,
     `packaging/make-tarball.sh X.Y.Z` → `vortex-launcher-X.Y.Z-x86_64-linux.tar.gz`.
   - `deb`: `debian:trixie` container, Rust via rustup (trixie's rustc is older than the MSRV),
     `cargo deb -p vortex-gtk --locked` → `vortex-launcher_X.Y.Z-1_amd64.deb`.
   - `rpm`: `fedora:latest` container, `cargo generate-rpm -p crates/vortex-gtk` →
     `vortex-launcher-X.Y.Z-1.x86_64.rpm`.
   - `smoke`: after the three builds, `packaging/smoke.sh` runs in a matrix of containers
     (debian:trixie and ubuntu:25.04 with the deb, fedora:latest with the rpm, archlinux and
     debian:trixie with the tarball): installs the package, checks `ldd` has no `not found`, that
     `.desktop`, metainfo and icons are installed and validate, and that the binary run as non-root
     without a display exits with GTK's "Failed to open display" and no loader/symbol error.
     `publish` needs it, so a failure leaves the release a draft. A failure means the package is
     broken on that distro (missing dependency, glibc/GTK too new for the target, missing file):
     fix in a normal PR and cut a patch release. Run it locally with
     `docker run --rm -v DIR:/pkg:ro -v $PWD/packaging/smoke.sh:/smoke.sh:ro IMAGE sh /smoke.sh KIND /pkg`
     (KIND is `deb`, `rpm`, `tar` or `tar-debian`).
   - `publish`: downloads the three, writes `sha256sums.txt`, creates a build provenance
     attestation (`actions/attest-build-provenance`), runs `gh release upload --clobber`, and
     only then publishes the draft (`gh release edit vX.Y.Z --draft=false`). So a public release
     always has its packages.

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
   `version` lines), `.release-please-manifest.json`, `version.txt`, `CHANGELOG.md`. Nothing else. Then push a commit to the release PR branch adding
   `<release version="X.Y.Z" date="..."/>` to `data/io.github.nexusdeveloper902.Vortex.metainfo.xml`
   (the one allowed extra file).
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
gh attestation verify /tmp/vortex-rel/vortex-launcher_*.deb --repo Nexusdeveloper902/Vortex
```

Report the run URL and the asset list, and that `gh release view vX.Y.Z --json isDraft` is
`false`. If a build job fails, the release stays a draft (tag exists, no or partial assets, not
public): fix the cause in a normal PR, then re-run the failed jobs of that workflow run
(`gh run rerun "$id" --failed`). The build jobs check out the tag, so a fix merged after the tag is
not in the rerun; if the fix must be in the artifacts, cut a new patch release instead.

Manual fallback, only if the owner asks: pushing a `vX.Y.Z` tag runs the build jobs for that tag
and uploads to the existing release of that name (`gh release upload` needs the release to exist,
draft or not), then publishes it.

## Packages and names

| Asset | Built on | Target | Name / binary |
|---|---|---|---|
| tarball | Arch (latest) | glibc ≥ 2.41 (verified on Debian 13), GTK ≥ 4.14, libadwaita ≥ 1.6 | `bin/vortex-launcher` |
| .deb | Debian 13 trixie | Debian 13+, Ubuntu 25.04+ | `vortex-launcher`, `/usr/bin/vortex-launcher` |
| .rpm | Fedora latest | Fedora | `vortex-launcher`, `/usr/bin/vortex-launcher` |
| PKGBUILD (not published) | user's machine | Arch | `vortex-launcher-git`, `/usr/bin/vortex-launcher` |

- Debian 12 and Ubuntu 24.04 are too old (GTK/libadwaita). Do not lower the deb build image.
- One name for every installed artifact: package `vortex-launcher` (Arch: `vortex-launcher-git`),
  cargo bin target and `/usr/bin/vortex-launcher`, `Exec`/`TryExec=vortex-launcher`, metainfo
  `<binary>vortex-launcher</binary>`. `vortex` is an unrelated app (Nexus Mods) on the AUR and
  owns `/usr/bin/vortex`. Only the app id `io.github.nexusdeveloper902.Vortex` and the data dir
  (`~/.local/share/vortex`) keep the short name. Per-game `.desktop` entries use `current_exe()`.
- Renaming the binary is user-visible but is committed as `feat`, not `!`: before 1.0 a `!`
  would make release-please jump to 1.0.0 (`bump-minor-pre-major` is off).
- The `<releases>` list in `data/*.metainfo.xml` is not updated by release-please: add a
  `<release version date/>` entry in the release PR (checklist item 2).
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
  `include-component-in-tag: false`, `draft: true`, `force-tag-creation: true`, `extra-files` bumps `$.workspace.package.version` in
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
