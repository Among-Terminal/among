# Supplemental Rust notice evidence

These are verbatim upstream inputs, not replacement license templates. Each
`index.json` entry addresses one exact published registry crate whose normal
license-file scan is insufficient. No package is exempted from the notice gate.
Inputs live under the already-delivered `THIRD_PARTY` tree so source snapshots and
notice payloads retain them without a separate native/source inventory change.
The generator writes only `texts`, `provenance` and `inventory.json`, never `inputs`.

| Published crate | Recorded upstream commit | Package path |
|---|---|---|
| asn1-rs-impl 0.2.0 | `a20e5f7319c896737ad0f2557037817b91ad854f` | `rusticata/asn1-rs`, `impl` |
| jni 0.22.4 | `5ae9458a4ec44c5318f37ddc7569c1d4ae8a69e7` | `jni-rs/jni-rs`, `crates/jni` |
| jni-macros 0.22.4 | `33045a124105c939d1e2cbdcb5a39e5d868ffa03` | `jni-rs/jni-rs`, `crates/jni-macros` |
| jni-sys-macros 0.4.1 | `64d77b7a5f119d7b55b4e2c169a4668067ff59e6` | `jni-rs/jni-sys`, `jni-sys-macros` |
| r-efi 6.0.0 | `7e1b0322d31d625f81a5656096330934f9cd835d` | `r-efi/r-efi`, repository root |
| rustls-platform-verifier-android 0.2.0 | `252e25161a91af476cbea620e29d277592d27ec7` | `rustls/rustls-platform-verifier`, `android-release-support` |

## Provenance and renewal

The exact `.crate` SHA-256, source registry, version, repository, declared license,
`.cargo_vcs_info.json` hash, recorded commit/path and every retained file's SHA-256
are in `index.json`. The archive hashes match the locked graph. Inputs came from
the corresponding GitHub repository's immutable contents API at that commit, not
its current branch. The generated inventory records a commit-pinned upstream URL
for every file. GitHub origin retrieval and these integrity checks are not a
cryptographic publisher signature or legal opinion.

`package-Cargo.toml` is the upstream package manifest; its bytes equal the exact
published `Cargo.toml.orig`. `Cargo.toml` is the upstream root/workspace manifest
(the same file for root package `r-efi`). In particular, `jni` inherits its license
from the retained workspace manifest. Preserve these source inputs so the scope
of a root license is reviewable, rather than relying only on a crate's SPDX string.

The five workspace packages retain both actual upstream MIT and Apache texts.
`r-efi` instead publishes its grant and original copyrights in `AUTHORS`; the
upstream commit's copy equals the published file. Its declared
`MIT OR Apache-2.0 OR LGPL-2.1-or-later` is preserved. `AUTHORS` supplies the actual
full MIT grant and alternative Apache/LGPL notices; this is not a claim that a
standalone full LGPL text was supplied or that every OR branch is selected.
No author's name, grant or attribution has been synthesized.

After a dependency update, inspect the new authenticated archive and its own
recorded VCS origin, retrieve and review the applicable source/license files,
and update the exact receipt and bytes together. Do not move a receipt to a new
version or replace its files with generic SPDX boilerplate. An unused or mismatched
receipt fails so that stale evidence cannot silently satisfy another graph.

## Offline consumer

Both `among-tools third-party-notices . THIRD_PARTY generate` and `check` use
`cargo metadata --locked --offline` for all three packages. The consumer checks:

- consistent archive checksums in the three lockfiles and exact metadata identity;
- actual registry-cache archive SHA-256, or cargo-vendor's matching package receipt;
- current published VCS bytes/commit/path and byte-identical upstream package source;
- bounded, nonempty, regular input files, safe relative paths, no symlinks, unique
  package receipts, source paths and output contents, and every input digest.

It includes verbatim license bytes in `THIRD_PARTY/texts`, source manifests in
`THIRD_PARTY/provenance`, and their origin/hash association in `inventory.json`.
It builds and validates the whole inventory before any output write. A later disk
failure can still interrupt multi-file generation; `check` must pass on the final
tree before distribution. Hashes bind reviewed input bytes; they do not themselves
decide whether an arbitrary text grants a license.

Run the actual consumer regressions with:

```sh
cargo test --locked --offline --manifest-path tools/among-tools/Cargo.toml --test notices
```

These inputs concern the locked Rust crate graphs only. They do not establish
AppImage/Snap native media inventories, corresponding-source delivery, runtime
acceptance or publication permission.
