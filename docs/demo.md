# Isolated demonstration capture

The demonstration films are frozen artifacts of earlier releases. The capture
harness that produced them is release tooling rather than part of the
application, so it is not part of this repository, and nothing here is a
supported way to re-record them. The Rust preparation, review, export,
assembly and clip-inspection commands below remain available and operate on an
already captured directory.

The capture harness records a newly allocated Xephyr display, not the personal
desktop. Only Xephyr receives parent display authorization. GNOME Shell, the
application, shell, plugins and ffmpeg receive a minimal explicit environment
with private HOME/XDG directories, no SSH agent or inherited credentials, a
private D-Bus and synthetic `atlas`/`orbit` projects. The shell reads only the
generated demo rcfile, never personal startup files. The mouse cursor is
explicitly included by x11grab's `-draw_mouse 1`.

## Existing prerequisites

Build both packages with the normal locked commands. Xephyr, GNOME Shell,
dbus-run-session, ffmpeg/ffprobe, xdotool, xprop and ImageMagick must already be
installed. The harness refuses missing tools; it does not install host packages.
The feature clips also require existing zsh, Starship, htop, mc, git and fc-match.
Run from a graphical X11 session, with no publication credentials required:

```sh
cargo build --release --locked
cargo build --release --locked --manifest-path examples/plugins/Cargo.toml
```

The smoke drives actual terminal input and a split, records a short silent MP4,
and checks the encoded stream/duration. It is **not** the public feature gallery.
Its evidence, private configuration, application log and isolation inventory are
retained under the printed `/tmp/opencode/among-demo-*` directory. The harness
closes only its own application/server/window-manager process groups.

## Feature clips without subtitles

```sh
among-tools check-demo-clips <captured-directory> <new-private-evidence-directory>
```

The driver records each feature separately through actual keys and mouse actions:
themes, splits, keyboard/mouse resize, pane drag, focus, zoom, fullscreen,
workspace creation/rename/switching, sidebar, all three plugin switches,
consented restoration of separate directories and pane closing/sibling input.
Missed actions abort the take rather than encoding a fake success. Four restored
shell PIDs and their real `/proc/<pid>/cwd` are compared with the saved snapshot.

Generated private zsh/Starship configuration provides colours and an ASCII project/git
prompt without executing personal startup files. Only read-only GNOME Terminal
palette and desktop monospace appearance values are copied. The chosen font is
copied to private XDG fonts and resolved there with fc-match; private VTE CSS
uses that family/size. This is demonstration styling, not a claim that Among
automatically imports GNOME Terminal profiles. `ll` uses numeric ownership;
MC disables its username/hostname xterm title under `[Layout]` and its subshell;
htop uses an owned PID filter and a generated screen without a USER column.

Theme escape sequences run in the generated startup file, never as visible
terminal input. The prompt uses plain `git:` and `>` markers instead of special
Unicode glyphs; actual tool interfaces retain their normal rendering.

Only shortcut events actually sent by the driver receive a maximum 1.6-second
highlight: bold white text on a rounded black badge centred in the video frame.
ImageMagick creates transparent-corner badge PNGs; ffmpeg composites them at
`(W-w)/2, (H-h)/2`. Highlights do not overlap one another. There are
no explanatory subtitles or subtitle stream, and mouse-only clips have no text
overlay. The first combined appearance clip shows light/dark; later clips force
dark. A fresh take remains a draft until visual/privacy/playback review.

The checker
This rejects partial clips, mismatched restoration directories/PIDs, GTK failures,
extra streams/metadata and invalid timings, then decodes every file and generates
two-frames-per-second contact sheets plus full-size review frames. It does not
approve privacy or copy drafts into README by itself. Theme halves are joined
into `01-themes.mp4`; subsequent clips each cover a separate feature. Failed
takes remain evidence and never replace the reviewed repository media.

## Reviewed local gallery

The [documentation overview](overview.md) is assembled from the exact reviewed gallery inputs:

```sh
among-tools assemble-demo <reviewed-clip-directory> /tmp/opencode/among-readme-overview
```

The assembler checks each input hash, keeps every clip at its original speed,
and joins scenes with direct cuts, without fades or black transition frames. It adds no titles, captions,
audio or invented shortcut events. `media/overview-manifest.json` records source
hashes, scene timings and output hashes. Decode, visually inspect and play the
result in a browser before replacing documentation assets. The user-facing
[feature guide](features.md) pairs controls and caveats with individual videos.

[The feature gallery](media/features/index.md) contains 13 clips of 3.52–17.52
seconds, 1600x1200 H.264/yuv420p with no audio/subtitle streams. Videos total about
2.4 MB. `media/features/manifest.json` records exact MP4/poster hashes and review
scope. The complete files decode; all 20 two-frames-per-second timeline sheets
and full-size tool/plugin/restoration/closing/shortcut frames were inspected.
Only synthetic projects/capture paths appear; no personal windows, username,
hostname or command lines were found. Raw logs, configuration and the copied
font stay private and are not gallery attachments. Browser playback/link
rehearsal is recorded in the change's verification notes. GitHub inline playback
is not assumed: previews link to downloadable MP4s and separate text descriptions.
These assets are local; no repository push or video upload occurred.

## Superseded single recording

The export tooling retains the earlier 88-second captioned
workflow and `docs/media/among-terminal-demo.*` evidence. The owner superseded
that presentation; it is no longer the README demonstration. Do not mistake its
historical encoding/playback checks for approval of new feature recordings.
