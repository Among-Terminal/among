# Fedora RPM packaging

No GitHub release carries an RPM yet. The RPM workflow builds and checks
candidates for release tags and keeps them as internal Actions artifacts only;
attaching them to a release is separate, approved work. Until a release lists
RPM assets, the download and installation sections below describe the package
the workflow builds, not a file you can fetch.

## GitHub release builds only

`.github/workflows/rpm-release.yml` builds candidates exclusively for `v*` tag
pushes. The tag's base version must equal the Cargo version; preview tags
`vX.Y.Z-rc.N` become RPM version `X.Y.Z~rc.N`, which RPM orders before the
stable `X.Y.Z`. There is no branch, pull-request, schedule or manual trigger.
Each target builds on a native GitHub runner (`ubuntu-24.04` for amd64,
`ubuntu-24.04-arm` for arm64) inside a Fedora container pinned by digest, with
Rust 1.98.1 installed by a checksum-verified rustup. `rpmbuild -ba` runs as an
unprivileged user and produces exactly one binary RPM and one SRPM.

Pull requests and `main` pushes run only offline checks:
`among-tools check-rpm-spec .` and the `rpm`, `rpm_workflow`, `rpm_payload`
and `rpm_installed` tests. No package is built locally; do not register host
QEMU/binfmt to build the other architecture.

## What the build job checks

- The spec (`packaging/rpm/among.spec`): one value for each single-valued
  header, no architecture tag, no path or plugin listed twice, the package owns
  `/usr/share/among`, and `LICENSE`, `NOTICE` and `THIRD_PARTY` exist in the
  tree and are packaged with `%license`. `%doc` files are skipped by
  `tsflags=nodocs`, which Fedora container images set.
- Library floors: `pkgconfig(gtk4) >= 4.12`, `pkgconfig(libadwaita-1) >= 1.5`
  and `pkgconfig(vte-2.91-gtk4) >= 0.76` may not be lower than the API level
  the Rust bindings are compiled for (the highest `vX_Y` feature of `gtk4`,
  `libadwaita` and `vte4` in `Cargo.toml`). The linked executable does not carry
  these minimums itself: GTK 4 and VTE export unversioned symbols and
  libadwaita one `LIBADWAITA_1_0` node for all 1.x releases. Each target
  records the library versions it actually linked against in
  `build-environment.txt`.
- `Source0` holds exactly the files of the tagged commit, without `.github/`
  and Git metadata files, and nothing untracked, private or built.
- Package and ELF architecture agree with the runner; no development
  artifact, internal tool, shared object or undeclared path is in the payload;
  every generated shared-library requirement has a provider in the Fedora
  repositories.
- The packaged `LICENSE`, `NOTICE` and `THIRD_PARTY` are byte-identical to the
  tag, and the notice texts match the three locked Cargo graphs. Native
  libraries are Fedora's packages and carry their own licenses; none is
  shipped. The notice inventory covers all three Cargo graphs, which includes
  crates of the release tooling that the package does not contain.

## Installed checks in release CI

A separate job installs the exact candidate with `dnf` in a fresh container of
the same target on a native runner. It refuses a candidate whose tag, source
commit, target, architecture, file name or SHA-256 differ from the build
receipt, and refuses compilers, `rpm-build` and `-devel` packages before and
after installation.

As an unprivileged user in a private Xvfb/Openbox session, against
`/usr/bin/among`, it then runs: the system desktop entry launched through GIO
with application ID, icon and shell input; focus; keyboard divider resizing in
all four directions; preferences with the packaged example plugins copied into
the user's own plugin directory; plugin status; and consented session
restoration with a restored split layout, a pane command that keeps running
while its workspace is hidden, and a pointer pane move.

The lifecycle check then installs an older package, upgrades, removes,
reinstalls and erases. The older package is the candidate's own payload
repacked in the build job as version `<version>~ciolder` with one extra
older-only file; it is not a historical release and never a release asset. It
travels in `ci-lifecycle-fixture-not-for-release/` and is bound to the receipt
by SHA-256. Throughout, synthetic preferences, shortcuts, a session, an
external plugin, `.bashrc`, `.zshrc`, Cargo and per-user launchers and a
per-user desktop entry must keep their hashes, and a separate user process
must stay alive. After the upgrade the older-only file must be gone and
`rpm -V` must pass against the candidate's header; after remove and after
erase no path the package owned may remain.

`runtime-record.json` names the package and fixture digests and every executed
check, and states `release_acceptance: false`.

## Choosing the package for your system

| Target | Package |
|---|---|
| `fedora43` / `amd64` | `among-<version>-1.fc43.x86_64.rpm` |
| `fedora43` / `arm64` | `among-<version>-1.fc43.aarch64.rpm` |
| `fedora44` / `amd64` | `among-<version>-1.fc44.x86_64.rpm` |
| `fedora44` / `arm64` | `among-<version>-1.fc44.aarch64.rpm` |

`rpm -E %fedora` prints your Fedora release and `uname -m` your architecture
(`x86_64` is amd64, `aarch64` is arm64). For `v0.1.0-rc.10`, `<version>` is
`0.1.0~rc.10`. A package for one Fedora release is not a substitute on
another.

## Installing, upgrading, rolling back and removing

```sh
sudo dnf install ./among-<version>-1.fc43.x86_64.rpm
```

`dnf` resolves GTK 4, libadwaita, GTK 4 VTE and the icon and settings data from
the Fedora repositories. There is no RPM repository or Copr project for Among
Terminal, so `dnf upgrade` never moves to a new version by itself; a release is
upgraded by installing its newer local file:

```sh
sudo dnf upgrade ./among-<newer-version>-1.fc43.x86_64.rpm
```

Rollback is a downgrade to an older local file:

```sh
sudo dnf downgrade ./among-<older-version>-1.fc43.x86_64.rpm
```

Remove the package through `dnf`, or erase it with `rpm` directly:

```sh
sudo dnf remove among
sudo rpm -e among
```

None of these commands touch your preferences, sessions, installed plugins or
shell startup files; they live in your home directory and belong to the
application, not to the package. The package has no scripts that stop
processes; the release check keeps a separate user process alive through every
operation, but does not keep an Among window open while doing so. Close and
reopen the application to use the newly installed version.

These are the operations the installed check runs, with `-y` added there.

### Coexisting with other copies

The package installs `/usr/bin/among`, its desktop entry, icon, AppStream
metadata, the example plugins under `/usr/share/among/plugins` and license
materials under `/usr/share/licenses/among`. A Cargo-built `among`, the
per-user source installation or a Snap are separate files and remain after the
package is removed. To start the system package explicitly:

```sh
/usr/bin/among
```

Coexistence with a real Snap installation is not exercised by the release
checks.

## What has actually been tested

Package construction and installed checks run only on GitHub release runners,
once per target. The display backend there is X11 under Xvfb with Openbox;
GNOME and Wayland sessions are not covered. Nothing here is a claim about
Fedora releases or architectures outside the table above.
