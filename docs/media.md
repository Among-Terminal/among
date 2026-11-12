# Documentation media

Screenshots and demo recordings for the site and README come from one
scene. The scene is real application state: a saved session, three named
workspaces, real PTYs, the real Preferences window and a real GNOME
notification. Nothing is assembled by hand.

The same command produces the files, so a layout change is fixed once instead
of once per page.

Build the display image once; it carries Xvfb, Xephyr, GNOME Shell,
ImageMagick, ffmpeg and `xsetroot`:

```sh
docker build -f tools/among-tools/containers/Dockerfile.display -t among-display:media .
```

## One command

Build the application test binary in the pinned Ubuntu image, build
`among-tools`, then run the capture inside the display image:

```sh
# 0. Prepare the bind mounts and the offline registry cache.
mkdir -p "$HOME/.cache/among/media/cargo" \
  "$HOME/.cache/among/media/build/target" \
  "$HOME/.cache/among/media/run"
cargo fetch --locked

# 1. Build the application test binary and the plugin fixture.
docker run --rm --network none --user "$(id -u):$(id -g)" \
  --mount type=bind,src="$PWD",dst=/source,readonly \
  --mount type=bind,src="$HOME/.cache/among/media/cargo",dst=/cargo \
  --mount type=bind,src="$HOME/.cargo/registry",dst=/cargo/registry,readonly \
  --mount type=bind,src="$HOME/.cache/among/media/build/target",dst=/target \
  --env HOME=/tmp --env CARGO_HOME=/cargo --env CARGO_TARGET_DIR=/target \
  --env AMONG_BUILD_VERSION=0.2.0 \
  --env AMONG_BUILD_COMMIT="$(git rev-parse --short HEAD)" \
  --env AMONG_BUILD_DATE="$(date -u +%F)" \
  --env CARGO_NET_OFFLINE=true --workdir /source \
  among-managed-resource-gui:1.98.1-ubuntu24-20261025-01 \
  bash -c 'cargo test --locked --offline --bin among --no-run --message-format=json > /target/test.jsonl && cargo build --locked --offline --example fake-plugin'
# Take the last compiler-artifact executable from target/test.jsonl; it is
# usually target/debug/deps/among-<hash>.

# 2. Build the tool.
cargo build --locked --release --manifest-path tools/among-tools/Cargo.toml

# 3. Capture and prepare everything.
docker run --rm --network none --user "$(id -u):$(id -g)" \
  --mount type=bind,src="$HOME/.cache/among/media/build/target",dst=/target,readonly \
  --mount type=bind,src="$PWD/tools/among-tools/target/release/among-tools",dst=/tools/among-tools,readonly \
  --mount type=bind,src="$PWD",dst=/source,readonly \
  --mount type=bind,src="$HOME/.cache/among/media/run",dst=/run \
  --env HOME=/run --env XDG_DATA_HOME=/run/data --env XDG_CONFIG_HOME=/run/config \
  --env XDG_CACHE_HOME=/run/cache --env XDG_STATE_HOME=/run/state \
  --env XDG_RUNTIME_DIR=/run --env AMONG_CHECK_ROOT=/run \
  --env AMONG_MEDIA_SOURCE="$(git rev-parse HEAD)" \
  --workdir /source among-display:media \
  /tools/among-tools media all /target/debug/deps/among-<hash> /run/out

# 4. Check the published contract inside the image that carries ffprobe.
docker run --rm --network none --user "$(id -u):$(id -g)" \
  --mount type=bind,src="$PWD/tools/among-tools/target/release/among-tools",dst=/tools/among-tools,readonly \
  --mount type=bind,src="$HOME/.cache/among/media/run",dst=/run \
  among-display:media \
  /tools/among-tools media verify /run/out
```

CI runs the same `media all` and `media verify` in the x86_64 native job and
keeps the output as the `documentation-media` artifact.

`among-display:media` is built from
`tools/among-tools/containers/Dockerfile.display`. It contains the owned X11
and GNOME stack, ImageMagick, ffmpeg and `xsetroot`; the image also owns
`/work`, the synthetic project path that may appear in a notification.

The captures write below the output directory and create the synthetic
`/work/among` project inside the disposable display image. The scene removes
that project only when its own `.git/among-media-project` marker is present;
a project without the marker is refused. `/work` is sticky and writable by
any runtime uid, so `--user "$(id -u):$(id -g)"` works without uid 1000.
`raw/` contains the untouched captures and stays as evidence. `media.json`
records every published file, its size, dimensions and SHA-256; set
`AMONG_MEDIA_SOURCE` to the source commit or the manifest keeps `source: null`
and prints a warning.

## Scenarios

| Command | What it captures |
| --- | --- |
| `media stills` | The restore prompt, the three-pane Workbench, an expanded pane, the Review workspace, the plugin status comparison and the seven Preferences pages including About |
| `media panes` | One pane split to five, divider resizes, a pane dragged to a new position, workspace switching, then a new workspace created and renamed |
| `media notification` | The Review workspace with a status terminal, a real GNOME notification, a pointer click and the focused terminal after it |
| `media all` | All three, in one output directory |

The stills and panes runs use an owned 1600x1000 Xvfb/Openbox display with the
application maximized, so the product fills the frame. The notification run
uses an owned Xephyr/GNOME Shell display with a private session bus, a private
desktop identity and dark system settings. All use the same project, terminal
colours, font fixture and application build.

## The published contract

| Asset | Size |
| --- | --- |
| `main-window.png`, `expanded-pane.png`, `workspaces.png` | 1600x1000 |
| `plugin-off.png`, `plugin-on.png` | 1600x1000 |
| `panes.mp4`, `panes-poster.png` | 1600x1000, H.264, 30 fps, no audio |
| `session-restore.png` | 800x480 |
| Preferences stills | 882 wide, 2 px taller than the window |
| `notification-return.mp4` | 1600x1000, H.264, 30 fps, no audio |
| `notification-return-poster.png` | 1600x1000 |
| `desktop-notification.png` | 640x130 |

The application fills the 1600x1000 display. Preferences stills are cropped to
the window with a 1 px `#3a414d` border, so no desktop margin remains. The
application is Adwaita dark, the terminal is Gray on black with the Tango
palette, and every PNG has its metadata stripped. `media verify` checks the
exact 18-name set, hashes, sizes,
and the video stream geometry and rate.

## The notification recording

The scene queues a bell in the unfocused `status` terminal, focuses a neutral
1x1 window so the application is in the background, and records the real
GNOME banner with `ffmpeg x11grab`. The pointer gesture is visible. GNOME's
X11 stage in this owned session does not deliver synthetic XTest clicks to the
banner, so the scene then calls the same `app.focus-pane` action the
notification's default target names. The payload and the real activation
contract are checked separately by
`among-tools check-desktop-notifications`; the recording is a visual
demonstration, not the notification acceptance test.

The shell's own startup notifications are suppressed by keeping banners off
until the scene is ready. They are not part of the product story.

The scene leaves Review for Workbench before recording, so the bell belongs to
the unseen status terminal and the banner click has a visible result. Recording
starts just before the bell, waits until the banner is visible, and captures the
poster from that confirmed frame. The clip contains the ring, the banner and the
return to Review without a hard-coded trim.

## The panes recording

The scene starts from a saved single-pane layout, splits to five with
`Ctrl+Shift+N`, drags the root divider twice, drags pane five onto pane two,
then switches to Review, Notes and back to Workbench with `Alt+2`, `Alt+3` and
`Alt+1`, creates a fourth workspace with `Ctrl+Shift+T` and renames it in
place. The project is synthetic; every terminal is real.

## The plugin comparison

`plugin-off.png` and `plugin-on.png` come from one stills run. The second image
registers the `github:12/demo-status` managed fixture, which answers the status
request with fixed `CPU 18%`, `MEM 6.2 GB` and `DISK 41%` fragments in the
application header. The fixture is a test executable, not a shipped plugin; the
values are synthetic and the page says so.

## Publishing

Copy the verified files into the site repository:

| Output | Site path |
| --- | --- |
| `main-window.png`, `expanded-pane.png`, `workspaces.png`, `session-restore.png` | `docs/public/media/app/` |
| `plugin-off.png`, `plugin-on.png` | `docs/public/media/app/` |
| `panes.mp4`, `panes-poster.png` | `docs/public/media/app/` |
| `notification-return.mp4`, `notification-return-poster.png`, `desktop-notification.png` | `docs/public/media/app/` |
| `plugins.png`, `plugin-access-review.png`, `notifications.png`, `keyboard.png`, `terminal.png`, `about.png` | `docs/public/media/preferences/` |

Use the dimensions above in page markup. Run the site's `npm run build` and
`npm run check` after replacing the files.

## Known limits

- The notification recording uses the owned GNOME X11 stage; it is not a
  Wayland or installed-package recording.
- The example project, plugin catalogue and package sizes are synthetic, as
  the site captions say.
- The raw captures are kept below the output directory; delete old output
  directories yourself when they are no longer needed.
- This pipeline covers documentation stills and the panes and notification
  clips. The promo, action and overview films keep their separate production
  tooling.
