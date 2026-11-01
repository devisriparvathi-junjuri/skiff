---
name: release
description: Cut a Skiff release. Use when the user asks to create, cut, or publish a release (for example "create the 0.6.2 release"). Bumps versions, tags, waits for CI, and publishes the draft.
---

# Release Skiff

A `v*` tag triggers `.github/workflows/release.yml`. CI builds the Linux packages
(AppImage, deb, rpm) and the macOS DMG. It attaches them to a **draft** release.

## Steps

1. Get the app version from the operator. Use the form `X.Y.Z`.
2. Check that the working tree is clean and you are on `main`. Check that
   `python3.12 scripts/host-boundary/check.py` passes before you change a version.
   If it fails, stop and report it.
3. Set the app version in `Cargo.toml` (`[workspace.package]`) and `apps/desktop/package.json`.
   Add `<release version="X.Y.Z" date="YYYY-MM-DD"/>` at the top of `<releases>` in
   `apps/desktop/src-tauri/linux/com.solutionscay.skiff.metainfo.xml`.
4. Bump skiffd only if the daemon changed. Find the last tag: `git describe --tags --abbrev=0`.
   If `git diff --quiet <tag> -- crates/skiffd crates/skiff-core` fails and the skiffd
   version is still the one at that tag (`git show <tag>:crates/skiffd/Cargo.toml`), raise
   the patch number of `version` in `crates/skiffd/Cargo.toml` and `crates/skiff-core/Cargo.toml`.
   Keep the two the same. Tell the operator the new skiffd version. A release with no
   skiffd bump asks no user to restart the daemon.
   Do not raise `PROTOCOL` in `crates/skiff-core/src/lib.rs` in a release. The change that
   breaks the wire raises it, under the rule written next to it. If `PROTOCOL` changed since
   the tag, tell the operator: users with live sessions must restart skiffd.
5. Refresh the lockfile: `cargo update --workspace --offline`.
6. Refresh the host-boundary hash of the files the version bump changed. The boundary
   check fingerprints `apps/desktop/package.json`, so the bump alone fails CI. Run
   `python3.12 scripts/host-boundary/check.py --refresh apps/desktop/package.json`
   (or `uv run --no-project --python 3.12 scripts/host-boundary/check.py --refresh ...`).
   Add `--refresh FILE` for each other file you changed in steps 3–5 that
   `scripts/host-boundary/baseline.json` lists; today no other one is listed.
   Refresh only these files. Never run `--record` in a release. The command must print
   `Host boundary passed`. If it lists another file, stop and report it: that change was
   not reviewed before the release.
7. Commit: `git commit -am "release: prepare vX.Y.Z"`.
   Run the full suite one time, on this commit, with `CARGO_BUILD_JOBS=8`:
   `cargo test --workspace`, all page tests (`cd apps/desktop && pnpm test`) and, on v2,
   `python3.12 scripts/host-boundary/check.py`. If a test fails, stop and report it.
8. Tag and push: `git tag vX.Y.Z && git push origin main vX.Y.Z`.
9. Wait for the run: `gh run watch` (about 10 minutes).
10. Check the assets: `gh release view vX.Y.Z --json isDraft,assets`.
   Expect `.AppImage`, `.deb`, `.rpm` and `.dmg`, each twice: once with the version in
   the name, and once as `Skiff_amd64.deb`, `Skiff_x86_64.rpm`, `Skiff_amd64.AppImage`,
   `Skiff_aarch64.dmg`. The website links to the second set.
   Also expect the updater's assets: `Skiff.app.tar.gz`, a `.sig` for it and for each
   versioned `.AppImage`, `.deb` and `.rpm`, and `latest.json`. Check that each URL in
   `latest.json` names an asset of this release. Without them, installed apps do not see
   the update.
11. Publish only when the operator says so: `gh release edit vX.Y.Z --draft=false --latest`.

## Rules

- Deleting old tags or releases needs explicit operator approval.
- If CI fails, report the failing job. Do not re-tag without asking.
