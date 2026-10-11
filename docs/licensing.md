# Licensing and distribution obligations

## Original work

Among Terminal's original host, independent example plugins, scripts and original
artwork use [Apache-2.0](../LICENSE). The owner authorized this transition during
release planning (recorded in `prepare-public-release/design.md`). Contributor
submissions are made under the same license unless explicitly agreed otherwise.
The AppStream metadata in `data/com.github.among-terminal.metainfo.xml` is explicitly
CC0-1.0 for catalog reuse; its project_license still identifies the application
as Apache-2.0. The original icon is Apache-2.0, not a copied theme asset.

The local provenance audit found no embedded third-party copyright/SPDX headers
or copied-source attribution in `src/` or `examples/plugins/src/`. Dependencies
are imported through Cargo and native dynamic linkage, not copied into these
modules. This directory has no Git history: the audit cannot reconstruct earlier
authorship independently. Owner authorization applies to original work, not to
third-party code, and new external contributions require a provenance review.

## Rust dependencies

Run `among-tools third-party-notices . THIRD_PARTY generate` after an intentional
lockfile update; run `among-tools third-party-notices . THIRD_PARTY check` before
distribution, using the prebuilt Rust tooling in an isolated container.
[The inventory](../THIRD_PARTY/inventory.json) covers all three locked Cargo graphs
(application, example plugins and independent tools),
including build-time and platform-specific packages. It preserves the declared
SPDX expressions and byte-for-byte upstream license/notice texts with SHA-256
digests; no synthetic attribution or blanket Apache label is applied.

The audited declarations permit MIT/Apache branches, with Unicode-3.0 and
LLVM-exception obligations retained where present. Ship `LICENSE`, `NOTICE` and
the complete `THIRD_PARTY` directory with source/binary artifacts. Inclusion in
the conservative inventory does not mean every package is linked on Linux.

## Native libraries are not relicensed

Rust `vte4` bindings are MIT; the native VTE library has separate LGPL-3.0-or-later
terms. GTK/libadwaita include LGPL-2.1-or-later components and additional notices.
Local evidence is the installed Debian package copyright files under
`/usr/share/doc/libgtk-4-1`, `libadwaita-1-0` and `libvte-2.91-gtk4-0`; native
VTE's version-matched source header is also identified in the release plan.
Those package documents can contain GPL tools/translations as well as the LGPL
library: review actual shipped files, not merely the package's aggregate label.

The application dynamically links these libraries. Apache licensing of original
code does not waive obligations for bundled LGPL libraries, including appropriate
notices, license copies, library modification/replacement, reverse engineering
for debugging modifications and applicable corresponding-source delivery.

## Snap release gate (not yet satisfied)

The actual core24 staged payload must be inventoried separately: installed local
Debian libraries are not proof of the Ubuntu Snap's contents. For each staged
component record exact version, file hashes, copyright/license texts, source
archive and packaging patches/build instructions. Provide required matching
source materials through a compliant delivery mechanism alongside each artifact;
do not treat a generic upstream download link as sufficient.

Inspect the dynamic loader and prove users can replace/debug modified LGPL
libraries without contaminating the host shell environment. Review any shipped
GPL programs/assets independently. Do not publish until this actual-payload
audit and installed-runtime checks pass. No Snap artifact or source-compliance
claim is established by the Cargo notices generator.

For a real builder's preserved `prime` and exact `stage_packages` Debian inputs:

```sh
among-tools inventory-snap-native <build-prime> <exact-stage-package-debs> <new-audit-directory>
```

Both directories must describe the same artifact. The command refuses a prime
that does not contain this project's own payload (`bin/among` and
`usr/share/doc/among/common-licenses-provider.txt`), and refuses any `.deb` that
contributes no file to that prime. Base-image packages are not staged in a
`core24` snap and must never be supplied here: auditing one produces licensing
failures that do not apply to the shipped payload.

The output records exact binary/source package versions, .deb hashes, observed
staged file hashes, differences from original .deb members and verbatim upstream
notices. It lists references to common license texts requiring delivery. It does
not fetch matching source archives, assert SPDX compatibility or establish LGPL
replacement/runtime compliance. It refuses missing inputs or an existing output
directory rather than substituting installed-host metadata or overwriting evidence.
Container fixtures: `cargo test --locked --manifest-path tools/among-tools/Cargo.toml --test native_inventory --test native_notices`.

Once exact source packages have been downloaded through authenticated Ubuntu APT
metadata, check their preserved inventory against the binary input inventory:

```sh
among-tools check-snap-sources <native-audit>/inventory.json <source-download-directory>
cargo test --locked --manifest-path tools/among-tools/Cargo.toml --test native_sources
```

The source directory must contain `inventory.json` with source identities, download
status and file sizes/SHA-256 hashes. The checker requires exact source-version
coverage, one matching `.dsc` per source and every archive referenced by its
`Checksums-Sha256`. Missing, changed, duplicate or symlinked inputs fail. It does
not extract archives, authenticate `.dsc` signatures, inspect patch/build adequacy
or assert legal compliance. Retain authenticated repository metadata and download
provenance separately; checksum consistency alone is not publisher authentication.

The Snap recipe now copies authentic Ubuntu common-license texts into the payload,
including the GPL/LGPL versions referenced by native copyright files, and records
the supplying `base-files` version. Verify those bytes in the actual rebuilt Snap:
editing the recipe is not evidence that an earlier artifact contains them.

```sh
among-tools check-snap-notices <native-audit>/inventory.json <actual-build-prime>
```

This check compares shipped native notices with inventoried upstream hashes and
requires every referenced common-license file to resolve inside the payload and
be nonempty. Its reported common-text hashes still need comparison with the
authentic Ubuntu provider; presence alone does not authenticate a license text.
Its real missing-text failure was reproduced on the earlier candidate. The
native-inventory fixture also exercises passing delivery, changed notices and an
escaping license symlink. These are specific delivery checks, not legal sign-off.

## Authenticated and artifact-linked source delivery

[Native audit](native-payload-audit.md) describes the retained linter warnings and
executed library/host-process evidence. [Source instructions](source-delivery.md)
describe offline original builds, Debian source extraction/patches, replacement
and debugging. The independent repository checker pins Ubuntu's archive key and
verifies signed InRelease, index hashes and every exact binary/source digest:

```sh
among-tools check-apt-provenance <signed-apt-metadata> <native-audit>/inventory.json <native-sources>
cargo test --locked --manifest-path tools/among-tools/Cargo.toml --test apt_provenance
```

Preserve metadata with `among-tools collect-apt-provenance <new-directory>` only
inside a fresh root Ubuntu 24.04 audit container. It enables source indices and
uses a dedicated APT lists directory; it changes that disposable container's APT
configuration, never the development host. Collection is not authentication:
run the pinned-key checker above against the exact native/source inventories.

`among-tools package-source-delivery` binds original/vendor source, native source/patches,
notices, common-license texts, authenticated metadata and builder records to one
Snap. `among-tools check-source-delivery` independently checks that association, archive
safety and every regular member hash without executing archive content. The
publisher must attach and retain the actual bundle, not just run the generator.
Local source packaging is not evidence that a public download already exists.
