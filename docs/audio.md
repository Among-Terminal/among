# Audio adapter

The independently testable `among::audio` adapter provides sound admission and
playback. General controls, preference persistence and bell-router integration
are separate work: this module alone does **not** add an audio settings page or
make terminal bells play a sound.

## Inputs and ownership

The original built-in sound is embedded from `data/audio/among-alert.wav`.
Its deterministic Rust generator and provenance are in `data/audio/README.md`.
User input is local RIFF/WAVE with uncompressed PCM8/16/24/32 or IEEE float32/64,
including matching WAVEFORMATEXTENSIBLE subtypes. Admission requires:

- At most 10 MiB, 10 seconds, two channels and 96 kHz; at least one frame/channel.
- Consistent sample/byte rates, block alignment, data size and valid-bit/mask fields.
- Complete RIFF/chunk extents and padding, one format/data chunk, at most 4096 chunks.
- Finite float samples. Compressed WAV, RIFX/RF64, nonregular files and malformed
  or unsupported inputs fail without executing a media helper chosen by the file.

Ancillary chunks are stripped. Accepted bytes are copied into a sealed memfd;
MediaFile opens its owned `/proc/self/fd` path, never the original user pathname.
The descriptor stays alive with playback. GTK 4.14's GStreamer input-stream path
asserts rather than accepting a GInputStream, so passing one is not a safe
alternative on the Ubuntu 24.04 baseline.

## API and lifecycle

Create one `Audio` owner after GTK initialization; a second concurrent owner is
rejected. `select_local` reads/validates off the GUI loop, with one process-wide
file-read slot. `select(Sound)` accepts an already validated immutable snapshot.
Selection cancels old playback; stale read completion cannot replace a newer
selection, run an obsolete completion callback or replay a sound after shutdown.
Failed selection leaves the last accepted source selected but never plays it.

`preview()` is explicit and independent of event admission. It does not call
notification ingress, write preferences or invoke commands/providers. The host
supplies current effective enablement/eligibility with `set_event_admitted`;
`event()` is suppressed unless admitted. Revoking admission cancels event audio,
not an explicit preview. Source removal/current-policy evaluation belongs to the
later router adapter; do not retain an old admission decision across focus changes.
`cancel_event(id)` cancels only the matching active event request, not a newer
event or a preview; retain its returned request ID with the originating live pane.

There is one active MediaFile and **zero queued playback requests**. Busy events
coalesce; explicit preview replaces the active sound. `cancel`, source change,
`shutdown` and owner drop stop/clear the owned stream. A 13-second monotonic
deadline bounds a decoder that never finishes; at most 32 bounded notices remain.
`Requested`/`DecoderEnded` report native lifecycle, not proven audible output.
The adapter checks decoder errors before ended/prepared state and never falls
back to VTE's independent beep.

## Packaging and evidence boundaries

Debian/Ubuntu explicitly require `libgtk-4-media-gstreamer` and
`gstreamer1.0-plugins-good` (which requires base). Fedora's `gtk4` supplies its
media implementation; `gstreamer1-plugins-good` supplies WAV parsing/pulsesink,
and `libglvnd-gles` supplies the media paintable's dynamically opened GLES library.

Snap stages the Ubuntu media/base/good route. AppImage preserves the GTK media
module, ten declared GStreamer plugins and plugin scanner in their nested paths;
linuxdeploy resolves/patches their dependency RUNPATHs before attribution.
`data/audio/runtime-files.rs` is shared by packaging and startup requirements.
Startup initializes private discovery with a fresh private GStreamer registry,
then restores every GTK/GStreamer override before host shells/plugins can start.
No payload `LD_LIBRARY_PATH` is exported. Missing media/codec/scanner members
fail explicitly instead of borrowing host backends to make a test appear green.

`examples/audio-probe.rs` consumes these actual APIs. It refuses playback outside
an owned device-free container with a confirmed private PulseAudio null sink.
Use captured signal, diagnostics and loaded-library maps for acceptance, not a
play request or `prepared=true`. Native adapter output does not establish real
General controls, an installed DEB/RPM, Snap/core24 or another architecture.
Every new artifact still needs exact-payload source/notices/replacement checks
and its own live-output run. See `docs/licensing.md` for native obligations.
