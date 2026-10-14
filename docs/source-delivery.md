# Source delivery, rebuilding and library modification

Publish `among-source-materials.tar` and `source-association.json` alongside the
exact Snap named by its SHA-256 in the association. Keep those source assets
available for every binary revision: a rolling nightly must not erase a previous
revision's corresponding-source access. Mirror/preserve per-commit source assets
before replacing a rolling preview. Do not substitute a generic upstream URL.
The bundle contains original source with locked vendored Rust crates, all matching
native Debian source descriptors/upstream archives/packaging patches, authentic
notices/common-license texts, payload file-change inventory and signed Ubuntu
repository metadata. It does not relicense these native sources to Apache.

## Verify and unpack

Compare both assets against the release provenance/association before extracting.
Inspect tar member names first; reject absolute paths, `..`, duplicate members,
devices and links. Unpack into a fresh private directory, never over an existing
project. Original source is a separate `original-source.tar` inside the bundle.
The inventory hashes cover regular file bytes; timestamps/users are normalized.

Rust integrity commands, run in isolated containers before extraction:

```sh
among-tools check-source-snapshot original-source.tar
among-tools check-source-delivery source-association.json among-source-materials.tar among_VERSION_ARCH.snap
among-tools check-stable-source /path/to/reviewed-tag-checkout provenance.json among-source-materials.tar
```

The first checks member safety, exact inventory hashes and mandatory source/vendor
inputs. The second binds exact binary/source digests. The third compares selected
original inputs to actual tag Git blobs and permits only the stable-grade recipe
overlay. None establishes full native-source compliance. Vendor all three locked
dependency graphs when constructing original source:

```sh
cargo vendor --locked --sync examples/plugins/Cargo.toml --sync tools/among-tools/Cargo.toml /path/to/new-vendor
among-tools source-snapshot . /path/to/new-vendor /path/to/new-original-source.tar
```

The snapshot takes exactly these inputs from the source root, the list
`source-snapshot` carries in `tools/among-tools/src/source_snapshot.rs`:

- files `Cargo.toml`, `Cargo.lock`, `examples/plugins/Cargo.toml`,
  `examples/plugins/Cargo.lock`, `LICENSE`, `NOTICE`, `README.md`,
  `CONTRIBUTING.md`, `examples/check-clipboard.rs`, `examples/fake-plugin.rs`
  and, for the project tooling, `tools/among-tools/Cargo.toml`,
  `tools/among-tools/Cargo.lock` and `tools/among-tools/README.md`;
- every regular file below `packaging`, `tests`, `src`, `data`, `snap`,
  `scripts`, `docs`, `THIRD_PARTY`, `.github`, `examples/plugins/src`,
  `examples/plugins/tests`, `examples/plugins/examples` and, for the project
  tooling, `tools/among-tools/src`, `tools/among-tools/tests` and
  `tools/among-tools/containers`, skipping `__pycache__` directories.

Each listed file must exist and a listed directory must not be a symlink. The
vendor tree is added under `vendor/`, then a generated `.cargo/config.toml`
that points Cargo at it and `source-inventory.json` with every member's
SHA-256. The tooling's license texts are inventoried with application and
plugin dependencies.
Script-by-script privacy review remains required before publicly distributing
a snapshot made from an internal working tree.

From the unpacked original source, authenticate native inputs again:

```sh
among-tools check-apt-provenance ../apt-authentication ../native-audit/inventory.json ../native-sources
```

The trust root is the Ubuntu Archive Automatic Signing Key (2018) fingerprint
`F6ECB3762474EDA9D21B7022871920D1991BC93C`, pinned in the checker, not a key chosen
by the source bundle. InRelease signatures cover each index's SHA-256; indices
cover exact `.deb`, `.dsc` and source archive digests. This authenticates Ubuntu's
publication without pretending to verify every separate uploader signature.

Use `among-tools check-snap-sources ../native-audit/inventory.json ../native-sources`
for exact inventory/descriptor checks without signature trust, and
`among-tools check-native-source-unpack ../native-sources ../native-unpack-audit`
inside a disposable container to exercise Debian source extraction and packaging
patches. The latter records control/rules hashes, not a full native package rebuild.
`among-tools check-snap-notices ../native-audit/inventory.json /path/to/prime`
compares authentic notice hashes and referenced nonempty common-license texts
against the actual prime payload. All these commands have deliberately bounded
scope; none is legal sign-off or a substitute for installed runtime acceptance.

Build the source association only after those audits, using the exact binary,
preserved original snapshot and builder evidence. Run inside the audit container:

```sh
among-tools package-source-delivery candidate.snap original-source.tar native-audit native-sources apt-authentication prime build-environment docs/source-delivery.md ../new-delivery
```

The command authenticates repository indices and input digests, compares native
notices against prime, and reads actual SquashFS executable/plugin/metadata bytes
before creating output. The archive uses regular, normalized members and resolves
only contained common-license aliases. `builder-python.txt` is preserved external
Snapcraft environment evidence, not a dependency of project tooling. These inputs
still need a public privacy review before distributing the resulting archive.

## Original application and independent plugins

Use Rust 1.98.1 and a native Ubuntu 24.04/amd64 build environment with pkg-config,
GTK4/libadwaita/VTE development packages matching the recipe. The snapshot's
`.cargo/config.toml` resolves dependencies from `vendor/`, not the network:

```sh
cargo build --release --locked --offline
cargo build --manifest-path examples/plugins/Cargo.toml --release --locked --offline
cargo build --manifest-path tools/among-tools/Cargo.toml --release --locked --offline
```

Run all three commands from the original-source directory so the
vendor configuration is discovered. Build/install changes only in a disposable
VM or container unless you intentionally approve changes to your own machine.
For the Snap, use the included recipe with Snapcraft 8.14.0, Rust 1.98.1 and
core24/amd64. APT, compiler tooling and Snapcraft's Python graph are not fully
hermetically pinned: a rebuild is not advertised as byte-identical. The actual
native-input inventory describes the binary being supplied, not future APT state.

## Native source and packaging patches

Every native source directory contains its version-matched `.dsc` and all members
named by `Checksums-Sha256`. Ubuntu packaging patches and `debian/rules` are in
the Debian source tar. In a disposable Ubuntu 24.04 environment, for each source:

```sh
dpkg-source --no-check -x <matching-package.dsc> <fresh-output-directory>
cd <fresh-output-directory>
sudo apt-get build-dep ./
dpkg-buildpackage -us -uc -b
```

`--no-check` skips the independent uploader signature only **after** the pinned
repository/hash checker passed. `dpkg-source` applies the distributed packaging
patches; `debian/control` defines dependencies and `debian/rules` the build. The
APT build-dependency step can install packages and requires your approval: run
it in the disposable environment, not on the personal host. This is a recipe for
rebuilding/modifying sources, not a claim that all native packages were rebuilt.

Among makes no native-library source patches. Snapcraft modifies staged ELF
interpreters/RUNPATHs for the core24 loader; `native-audit/inventory.json` records
original/staged hashes and every modified member. The included Snap recipe shows
schema/MIME/loader-cache generation and layout. Native notices and source files
retain their own licensing, including applicable GPL/LGPL terms.

## Replace and debug libraries without changing host shells

You may modify bundled libraries and reverse-engineer the combined executable
to debug those modifications. No Among term prohibits this. Classic confinement
does not restrict replacement to an approved plugin mechanism. Unpack the Snap
into a private directory (`unsquashfs -d <private-payload> <among.snap>`), place
your rebuilt ABI-compatible libraries in a separate private library directory,
and use the base's loader for **that application process only**:

```sh
/snap/core24/current/lib64/ld-linux-x86-64.so.2 \
  --library-path <modified-library-dir>:<private-payload>/usr/lib/x86_64-linux-gnu:/snap/core24/current/lib/x86_64-linux-gnu \
  <private-payload>/bin/among
```

Supply `SNAP_NAME=among` and `SNAP=<private-payload>` to that command if using
private schemas/icons/MIME data. The generated loader cache targets the installed
mount; regenerate it with gdk-pixbuf-query-loaders and private absolute paths
when testing a private SVG loader. Do not export LD_LIBRARY_PATH globally: that
would change host shell/tool behavior. Running this loader under GDB permits
breakpoints/backtraces in modified native libraries. An earlier automated
regression exercised a user-compiled dependency added to a private VTE
library, a GDB breakpoint and
uncontaminated child maps; the Rust `check-snap-runtime` port does not yet
accept `--replace-vte` and refuses it rather than reporting a pass. Historical
guest evidence of replacement mechanics is not a supported reproduction
command. This tests replacement mechanics, not every possible ABI-changing
modification.

Core24 is separately installed from Canonical, not bundled into Among's Snap.
Its own source/licensing distribution remains Canonical's; the Among payload
inventory covers the files Among actually distributes. Preserve base revision
and builder metadata with each release for environment traceability.
