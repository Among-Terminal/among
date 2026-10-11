# Build prerequisites

Built on Debian 13 (trixie), Rust 1.98.1, against GTK 4.18.6, libadwaita 1.7.6,
and VTE 0.80.1. Live acceptance runs use isolated Xephyr/native X11 with GNOME
Shell. A Wayland host desktop is not evidence of native Wayland application
behavior; Wayland, Snap and Flatpak runtime acceptance remains unexecuted.

## System packages

```sh
sudo apt install libgtk-4-dev libvte-2.91-gtk4-dev libadwaita-1-dev
```

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
