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

## Reusable packaged-output checks

The headless tooling binds an external probe to an actual artifact digest,
source commit and ELF architecture, separately from the shipped payload:

```sh
among-tools bind-audio-probe audio-probe candidate.AppImage SOURCE_SHA amd64 new-probe-directory
among-tools check-audio-package appimage candidate.AppImage new-probe-directory SOURCE_SHA amd64 new-evidence
```

Use only an owned device-free container. The checker refuses a visible native
GTK media/GStreamer plugin tree, creates a fresh private Unix PulseAudio socket
with exactly one null sink, confirms its identity before launching a consumer,
and retains raw PCM, waveform analysis, diagnostics, environment and module maps.
It generates original 880 Hz PCM8/16/24/32 and float32/64 fixtures. Each positive
must produce that signal; a disabled event and three missing-member fixtures
must produce nonempty silent baselines. The original artifact is never modified.
Probe copies alone receive discovery RUNPATH/interpreter changes; their before/
after digests are retained. Kernel, compiled and expected architectures must agree.
Hosted runner identity remains necessary to distinguish native arm64 from emulation.

`snap-core24` checks an extracted **actual Snap**, using its real interpreter
under an available `/snap/core24/` base. This establishes loader/discovery/output,
not snap-confine or installed launch. `snap-installed` additionally requires the
installed executable and full native-library/module/helper inventory to match
the artifact, and invokes the external probe through `snap run --shell among`.
It retains the actual SNAP revision, process security context, mount information
and core24 loader maps. Among currently declares **classic** confinement: this
must not be described as a strict sandbox. Missing-member negatives run in
separate extracted fixtures, not by altering the installed read-only Snap.

Existing AppImage and Snap workflows transfer these bound checkers for both
native runner architectures and retain failure captures. A workflow edit does
not establish output until that exact commit's hosted run has executed it.
These remain library-adapter checks, **not real General/VTE output**. The later
General-control driver can reuse `audio_capture::Invocation`: its executable
receives `(scenario, new-consumer-evidence, fixture-wav, payload-root)` inside the
owned sink environment. It must drive the installed application's real controls
and save that application's maps/diagnostics, rather than call `Audio` itself.
No alternate production test flag or General control is introduced here.
