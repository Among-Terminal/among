# Application artwork

`com.github.among-terminal.svg` is an unchanged copy of the owner-approved
`docs/media/logo-small.svg`. The approved raster counterpart is
`docs/media/logo-small.png` (559 × 559).

The 128 px and 256 px PNGs here are resized from that approved PNG with
GdkPixbuf 2.42.12, without adding a background or changing the artwork:

```sh
gdk-pixbuf-thumbnailer -s 128 docs/media/logo-small.png data/com.github.among-terminal-128.png
gdk-pixbuf-thumbnailer -s 256 docs/media/logo-small.png data/com.github.among-terminal-256.png
```

AppImage staging installs them in the matching `hicolor/128x128/apps` and
`hicolor/256x256/apps` directories. AppImageLauncher alpha-4/libappimage selects
those exact-size paths for its file thumbnails. On the tested Debian 13 system,
its SVG-only thumbnail path silently writes fully transparent PNGs even though
GNOME can render the installed SVG correctly. Merely changing `.DirIcon` while
keeping the scalable icon does not avoid that selection.

The artwork gate binds the approved inputs and these derived bytes to hashes.
AppImage assembly checks the raster files again after deployment and extraction;
the GNOME integration check decodes AppImageLauncher's actual output thumbnails
and rejects missing colour/visible pixels. Native icon names and SVG ownership
remain unchanged.
