# Native payload audit boundaries

The amd64 core24 candidate uses Ubuntu GTK 4.14.5, libadwaita 1.5.0 and VTE
0.76.0. Its exact binary/source versions and file hashes are in the artifact's
native inventory, not inferred from the developer's installed Debian packages.
The current inventory covers 87 Debian inputs and 64 source versions. Signed
Ubuntu metadata authenticates all input `.deb`, `.dsc` and source archives.
All 64 source packages were extracted with their packaging patches; every one
contains Debian control/rules build instructions. Full recompilation of every
native package is not claimed. Original host/examples were rebuilt offline from
an unpacked source snapshot with vendored locked crates and an empty Cargo home.

## Library linter warnings: retain, do not blindly strip

The classic linter passes. Snapcraft's static library linter reports five unused
shared libraries. They are retained as deliberately conservative package inputs:

| Library | Version-matched source evidence | Disposition |
| --- | --- | --- |
| libGLESv2.so.2 | libepoxy `src/dispatch_common.c` names this SONAME and passes it to `get_dlopen_handle` | Keep: actual dynamic dependency missed by DT_NEEDED-only scans. GPU/Wayland coverage remains unexecuted. |
| libcolordprivate.so.2 | colord `lib/colord/meson.build` builds the private library; public colord also includes `shared_src`, and the observed public ELF has no DT_NEEDED for the private library | Keep the distribution package's auxiliary library rather than changing upstream package composition based on a linter alone. |
| libdconf.so.1 | dconf `gsettings/meson.build` links common/thread build dependencies directly into dconfsettings; the independent client library is a separate target | Keep client API with the package; observed GSettings backend is not evidence of every external settings consumer. |
| libicuio.so.74 | ICU `source/io/Makefile.in` defines the supplementary I/O library | Keep the packaged supplementary API; it is not needed by observed terminal scenarios. |
| libicutest.so.74 | ICU test/tool build definitions include the test framework | Keep this packaged auxiliary library in the conservative initial distribution. It is not executed by Among's observed application scenarios. |

None has a direct DT_NEEDED consumer in the audited payload; this is a static
observation, not proof of absence of `dlopen` consumers. libGLES is explicitly a
counterexample. Retention does not exempt these files from source/notices delivery:
their full corresponding source versions, patches and licenses are included.
The remaining warnings are disclosed packaging overhead, not silently suppressed
or described as zero-warning lint. A future size-reduction change should test
each removal separately, including affected dynamic backends.

## Application-private discovery and host processes

Schema source, MIME directory and SVG loader are initialized before GTK caches
their availability; icon search paths are added only to the application display.
No bundled LD_LIBRARY_PATH, XDG_DATA_DIRS or loader override is exported into host
shells or external plugins. Installed acceptance drives a real Bash startup, a
typed host command and project directory, checks caller variables and child
memory maps, and renders a separately installed plugin. The modified-VTE/GDB test
uses a private library and confirms child maps remain free of payload/core24
libraries. Normal/narrow screenshots now show all three header symbols.

X11 amd64 split/keyboard and pointer resize/move/background/PWD/focus/window,
plugin containment and session suites passed. UTF-8 path bytes and tooltips pass,
but the minimal guest lacks proven CJK glyph coverage. Do not advertise Wayland,
GPU, arm64, Flatpak, screen-reader support or a strict sandbox based on this audit.
Among is classic-confinement; Store registration and classic approval remain
owner-only prerequisites, not actions performed by local packaging.

See [source delivery and modification instructions](source-delivery.md). Each
binary must have its own checked, digest-linked source bundle at publication.
Retain build environment records and do not claim hermetic/byte-identical builds
when APT/tooling inputs are not fully pinned.
