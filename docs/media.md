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
# 1. Build the application test binary.
docker run --rm --network none --user "$(id -u):$(id -g)" \
  --mount type=bind,src="$PWD",dst=/source,readonly \
  --mount type=bind,src="$HOME/.cache/among/media/cargo",dst=/cargo \
  --mount type=bind,src="$HOME/.cargo/registry",dst=/cargo/registry,readonly \
  --mount type=bind,src="$HOME/.cache/among/media/build/target",dst=/target \
  --env HOME=/tmp --env CARGO_HOME=/cargo --env CARGO_TARGET_DIR=/target \
  --env CARGO_NET_OFFLINE=true --workdir /source \
  among-managed-resource-gui:1.98.1-ubuntu24-20261025-01 \
  bash -c 'cargo test --locked --offline --bin among --no-run --message-format=json > /target/test.jsonl'
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
  --workdir /source among-display:media \
  /tools/among-tools media all /target/debug/deps/among-<hash> /run/out

# 4. Check the published contract.
tools/among-tools/target/release/among-tools media verify \
  "$HOME/.cache/among/media/run/out"
```

`among-display:media` is built from
`tools/among-tools/containers/Dockerfile.display`. It contains the owned X11
and GNOME stack, ImageMagick, ffmpeg and `xsetroot`; the image also owns
`/work`, the synthetic project path that may appear in a notification.

The stills and the recording write only below the output directory.
`raw/` contains the untouched captures and stays as evidence. `media.json`
records every published file, its size, dimensions and SHA-256.

## Scenarios

| Command | What it captures |
| --- | --- |
| `media stills` | The restore prompt, the three-pane Workbench, an expanded pane, the Review workspace, all six Preferences pages |
| `media notification` | The Review workspace with a status terminal, a real GNOME notification, a pointer click and the focused terminal after it |
| `media all` | Both, in one output directory |

The stills run under an owned 1600x1000 Xvfb/Openbox display. The notification
run uses an owned Xephyr/GNOME Shell display with a private session bus, a
private desktop identity and dark system settings. Both use the same project,
terminal colours, font fixture and application build.

## The published contract

| Asset | Size |
| --- | --- |
| `main-window.png`, `expanded-pane.png`, `workspaces.png` | 1600x1000 |
| `session-restore.png` | 800x480 |
| Preferences stills | 1040 wide, 160 px taller than the window |
| `notification-return.mp4` | 1600x1000, H.264, 30 fps, no audio |
| `notification-return-poster.png` | 1600x1000 |
| `desktop-notification.png` | 640x130 |

The backdrop is `#20262f`, the window border `#3a414d`, the application is
Adwaita dark, the terminal is Gray on black with the Tango palette, and every
PNG has its metadata stripped. `media verify` checks the names, hashes, sizes,
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

## Publishing

Copy the verified files into the site repository:

| Output | Site path |
| --- | --- |
| `main-window.png`, `expanded-pane.png`, `workspaces.png`, `session-restore.png` | `docs/public/media/app/` |
| `notification-return.mp4`, `notification-return-poster.png`, `desktop-notification.png` | `docs/public/media/app/` |
| `plugins.png`, `plugin-access-review.png`, `notifications.png`, `keyboard.png`, `terminal.png` | `docs/public/media/preferences/` |

Use the dimensions above in page markup. Run the site's `npm run build` and
`npm run check` after replacing the files.

## Known limits

- The notification recording uses the owned GNOME X11 stage; it is not a
  Wayland or installed-package recording.
- The example project, plugin catalogue and package sizes are synthetic, as
  the site captions say.
- The raw captures are kept below the output directory; delete old output
  directories yourself when they are no longer needed.
- This pipeline covers documentation stills and the notification clip. The
  promo, action and overview films keep their separate production tooling.
