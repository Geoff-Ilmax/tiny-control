# Releasing

Releases are produced entirely by
[`.github/workflows/release.yml`](.github/workflows/release.yml). Nothing is
uploaded by hand, and no install path in this repo tracks a mutable branch.

## Cutting a release

1. Bump `version` in `Cargo.toml` and refresh `Cargo.lock`
   (`cargo build --locked` will tell you if it is stale).
2. Update the `VER=` example in the README install section.
3. Commit, then tag and push:

   ```bash
   git tag -a v0.1.0 -m "tiny-control v0.1.0"
   git push origin main --follow-tags
   ```

The workflow refuses to publish if the tag and the `Cargo.toml` version
disagree.

## What a release contains

| Asset | Notes |
|---|---|
| `tiny-control-<ver>-x86_64-linux-musl.tar.gz` | Static binaries + installer + packaging files |
| `tiny-control-<ver>-aarch64-linux-musl.tar.gz` | Same, arm64 (built on a native arm64 runner) |
| `tiny-control_<ver>_{amd64,arm64}.deb` | Debian/Ubuntu |
| `tiny-control-<ver>-1.{x86_64,aarch64}.rpm` | Fedora/RHEL |
| `tiny-control-<ver>-1-{x86_64,aarch64}.pkg.tar.zst` | Arch |
| `tiny-control-<ver>.tar.gz` | Source tarball (`git archive` of the tag) |
| `PKGBUILD` | Rendered from `packaging/PKGBUILD.in`, pinning the source tarball by `sha256` |
| `SHA256SUMS` | Covers every asset above |

Distro packages are built with [nfpm](https://nfpm.goreleaser.com/) from
[`packaging/nfpm.yaml`](packaging/nfpm.yaml); nfpm itself is pinned to a version
and checksum-verified against its own published `checksums.txt` before use.

Every asset also gets a [Sigstore build-provenance
attestation](https://docs.github.com/actions/security-guides/using-artifact-attestations),
so a consumer can prove an artifact came from this repo's workflow at that tag:

```bash
gh attestation verify <asset> --repo Geoff-Ilmax/tiny-control
```

## Why the PKGBUILD pins a self-hosted tarball

GitHub's auto-generated `/archive/refs/tags/*.tar.gz` is regenerated on demand
and its bytes are not contractually stable, so pinning a `sha256` against it is
fragile — which is why so many PKGBUILDs end up with `sha256sums=('SKIP')`. This
project publishes its own `git archive` tarball as a release asset instead;
release assets are stored blobs, so the checksum in the PKGBUILD stays valid.

After each release the workflow commits the rendered `PKGBUILD` back to
`main` at [`packaging/PKGBUILD`](packaging/PKGBUILD), so the checked-in copy
always matches the newest published release. Edit
[`packaging/PKGBUILD.in`](packaging/PKGBUILD.in), never `packaging/PKGBUILD`.

## Building the packages locally

```bash
cargo build --release --locked --target x86_64-unknown-linux-musl
mkdir -p staging/current dist
cp target/x86_64-unknown-linux-musl/release/{tinyctl,tiny-guard} staging/current/
PKG_ARCH=amd64 PKG_VERSION=0.1.0 \
  nfpm package --config packaging/nfpm.yaml --packager deb --target dist/
```
