# Build prerequisites

Built on Debian 13 (trixie), Rust 1.98.1, against GTK 4.18.6, libadwaita 1.7.6,
and VTE 0.80.1. Live acceptance runs use isolated Xephyr/native X11 with GNOME
Shell. A Wayland host desktop is not evidence of native Wayland application
behavior; Wayland, Snap and Flatpak runtime acceptance remains unexecuted.

## System packages

```sh
sudo apt install libgtk-4-dev libvte-2.91-gtk4-dev libadwaita-1-dev
```

The selectable-audio adapter additionally needs the runtime media/codec route:
`libgtk-4-media-gstreamer` and `gstreamer1.0-plugins-good` on Debian/Ubuntu.
GTK's optional recommendations are not sufficient on a no-recommends host.
See [audio ownership and testing boundaries](audio.md).

Verify:

```sh
pkg-config --exists gtk4 libadwaita-1 vte-2.91-gtk4 && echo ok
```

The GTK4 VTE library must be **0.76 or newer**. Native context-menu dispatch
requires this version; check with `pkg-config --modversion vte-2.91-gtk4`.

### Do not install `libvte-2.91-dev`

That package provides the **GTK 3** build of VTE, whose pkg-config file is
`vte-2.91.pc` and whose libraries are `-lgtk-3 -lgdk-3`. The bindings used here are
generated against the **GTK 4** build, whose pkg-config file is `vte-2.91-gtk4`
and whose libraries are `-lvte-2.91-gtk4 -lgtk-4`.

Both can be installed side by side — they are different shared objects, not
conflicting packages — but only the `-gtk4` one satisfies the build.

## Rust dependencies

From `Cargo.toml`:

```toml
[dependencies]
gtk = { package = "gtk4", version = "0.11", features = ["v4_10", "v4_12"] }
adw = { package = "libadwaita", version = "0.9", features = ["v1_5"] }
vte = { package = "vte4", version = "0.10", features = ["v0_76"] }
glib = "0.22"
gio = "0.22"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
libc = "0.2"
```

Several of these constraints are not optional, and none of them are explained in
the `vte4` README:

### `gtk4` needs the `v4_10` feature

`vte4`'s generated code references `gtk::Accessible`. In `gtk4` 0.11 that type is
behind the `v4_10` feature, so a plain `gtk4 = "0.11"` dependency fails:

```
error[E0425]: cannot find type `Accessible` in crate `gtk`
```

The `v4_12` feature is this project's own requirement, not `vte4`'s: it exposes
`CssProvider::load_from_string`, which is how `data/among.css` is installed.

### `glib` and `gio` must be listed directly

`vte4` does not re-export them. `Pty::new_sync` takes a `vte::PtyFlags`, and
`spawn_async` takes a `glib::SpawnFlags` and an `Option<&gio::Cancellable>`, so
both crates have to be reachable by name.

### `libadwaita` must track `gtk4`

The gtk-rs crates are versioned as a set, and the `-sys` crates declare a `links`
value. Two crates cannot both claim `links = "gtk-4"`, so cargo refuses the
build outright:

```
package `gtk4-sys` links to the native library `gtk-4`, but it conflicts with a
previous package which links to `gtk-4` as well: package `gtk4-sys v0.9.0`
```

`libadwaita 0.7` depends on `gtk4-sys 0.9`; `gtk4 0.11` needs `gtk4-sys 0.11`.
`libadwaita 0.9` is the release that tracks `gtk4 0.11`. Whenever `gtk4` is
bumped, `libadwaita` must move with it.

## Cargo install from Git

This is a source build, not a precompiled download. Install Rust **1.98 or newer**,
a C compiler and `pkg-config`, plus the native development packages described above
(GTK **4.12+**, libadwaita **1.5+**, GTK4 VTE **0.76+**).
On Ubuntu 24.04 or Debian 13:

```sh
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev
cargo install --git https://github.com/Among-Terminal/among --locked among
among
```

Cargo builds the current development version and installs the executable in
its bin directory, normally `~/.cargo/bin` (which must be on `PATH`). This does
not install a desktop launcher, icon or example plugins. Repeat the install
command with `--force` to update; use `cargo uninstall among` to remove it.

## Verifying a clean build

```sh
cargo build --release --locked
cargo test --locked
```

`cargo test` runs without a display. The VTE binding helpers that touch GTK —
`ShortcutTrigger::parse_string`, `CallbackAction::new`, `GtkSnapshot` — assert
that GTK is initialised, so they are kept out of the test paths and exercised
through the running application instead.

## Runtime notes

- VTE's search helpers need the PCRE2 multiline flag `0x400`, which is defined in
  neither the crate nor VTE's public headers. Without it VTE emits a warning on
  every search call.
- `vte::Regex::for_search` and `vte::Regex::for_match` are different constructors
  with different purposes; using the wrong one triggers a GLib critical at runtime.
- `text_range` and `text_selected_full` are commented out in the generated
  bindings and are unavailable. `text_selected` is behind the `v0_70` feature, and
  going through the clipboard to read text is not reliable enough to build on.

## AppImage candidates

`.github/workflows/appimage-release.yml` builds AppImages on native amd64 and
arm64 runners inside the digest-pinned Ubuntu 24.04 image and starts them on
clean Debian 13, Ubuntu 24.04 and Fedora 43 hosts. The same steps run in a
disposable Ubuntu 24.04 container of the target architecture, as root, from a
checkout, after installing the packages listed in the workflow's first step
and Rust 1.98.1:

```sh
cargo build --release --locked
cargo build --release --locked --manifest-path examples/plugins/Cargo.toml
cargo build --release --locked --manifest-path tools/among-tools/Cargo.toml
tools=tools/among-tools/target/release/among-tools
mkdir binaries
cp target/release/among examples/plugins/target/release/clock \
  examples/plugins/target/release/system-status examples/plugins/target/release/pane-pwd binaries/
"$tools" stage-appimage . binaries tools/among-tools/target/release/among-apprun amd64 Among.AppDir
"$tools" fetch-appimage-tools amd64 appimage-tools
"$tools" build-appimage Among.AppDir appimage-tools amd64 "$("$tools" package-version .)" appimage-candidate
"$tools" collect-appimage-sources appimage-candidate/among-*-amd64.audit.json appimage-sources
```

Use `arm64` instead of `amd64` on an arm64 machine; the commands refuse a
foreign architecture. `fetch-appimage-tools` executes nothing and keeps only
helper bytes whose SHA-256 matches the reviewed pins. `build-appimage` copies
those helpers again and re-checks the copies before running them, runs
linuxdeploy with its GTK plugin and appimagetool with the pinned runtime, puts
the reviewed `AppRun` back where linuxdeploy wraps it (the wrapper would export
GTK search paths into every shell), refuses an image without compiled schemas,
MIME cache or SVG pixbuf loader, audits the bundled libraries and extracts the
finished image to repeat those checks. Every output directory must be new.
`collect-appimage-sources` changes the container's APT configuration, so it
refuses to run outside a root container.

Refresh the disposable build container's base runtime packages after APT update,
before installing build dependencies. An older library can satisfy a new
dependency's SONAME/minimum version while its exact source version has already
left the current source index. The workflow upgrades those container packages;
the audit still records every actual binary/source version. This is not a
hermetic base-image rebuild, and source collection must still fail rather than
substitute a different version when an exact source is unavailable.
