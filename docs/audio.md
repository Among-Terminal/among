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

`event_authorized(EffectAuthorization)` is the owned core/backend delivery route.
The caller first obtains the capability from its privately issued exact delivery
and matching core Authorization, releases core/backend borrows, then transfers
the owned value. It still requires event admission; it never renews the original
received-at +10-second event expiry. The dispatcher must assign that delivery
once, not replay clones as new operations. The older `event()` remains the legacy
admission-only route for unmigrated consumers; it is not upgraded by this API.
General's actual ingress migration and producer-to-output acceptance are separate.

There is one process-wide decoder slot and **zero queued playback requests**.
Busy requests are not retained. Preview cancels the old operation, but returns
`Busy` if its helper has not yet retired; the caller must explicitly request a
new preview after capacity returns. No accepted policy/selection change replays
that request. `cancel`, source change, revocation, `shutdown` and owner drop only
cancel their owned operation token on the GTK thread; they never join a native
decoder there. At most 32 bounded notices remain.
`Cancelled` is emitted only after confirmed helper/group retirement, not merely
after requesting it; unconfirmed cleanup emits `Failed` instead.

### Supervised native lifetime

GTK 4.14's `MediaFile::clear` can join a GStreamer thread without a timeout, and
construction can block before a GUI watchdog exists. MediaFile construction,
playback, clear and drop therefore live on the main thread of an owned decoder
**process**, not on a worker thread holding a GTK object. The same host executable
has a narrow early decoder dispatch; it does not initialize the application UI,
notifications, commands or plugins. An inherited sealed memfd is revalidated in
the helper; the original pathname is never reopened. This supervision boundary
is not a sandbox.

One fixed supervisor thread owns the child/process group and bounded nonblocking
pipes. Its 13-second operation deadline includes construction and native
retirement: an ended acknowledgement alone cannot report `DecoderEnded`.
The helper also checks an absolute monotonic deadline before construction/play,
so delayed OS launch or a native call returning after expiry cannot start output.
Each operation additionally owns a distinct shared-memory start authority, separate
from its immutable sound descriptor. It begins pending; the supervisor grants it
only after the real spawn returns, the owned process group is confirmed and the
protocol pipes are nonblocking. Cancellation revokes it directly, including while
spawn observation is delayed. After actual native construction the helper checks
revocation/deadline and atomically consumes its single-use grant immediately before
calling play. Revocation cannot be regranted; a stale operation retains a different
mapping and cannot authorize or revoke its replacement. The inherited descriptor
is marked close-on-exec before native scanner children can start.

For an authorized event, its exact immutable core binding is also stored with the
one writable operation token. The trusted operation route supplies the expected
binding; a received FD set cannot self-authenticate or replace that binding.
The separate core export contributes up to five read-only scopes plus one sealed
manifest, at most six read-only descriptors and 512 binding bytes. Its bounded
`EffectDescriptors` wrapper remains owned through the real spawn return, and
helper import restores CLOEXEC before any native scanner can start. The original
expiry, exact Job/source/origin and live terminal scopes remain attached through
preparation, spawn, grant, native construction, active output and retirement.

The helper finishes import, mappings, protocol and native preparation before
claim. Immediately after the one successful SeqCst operation CAS it calls
`validate_after_claim()` for ordered terminal-scope reads and fresh local monotonic
time, then directly plays the prepared media. Failure never plays/regrants/retries.
Active helper checks read the original core scope mappings without GUI/backend or
supervisor polling. The 13-second operation budget remains separate from the
original event deadline. This preserves parent-death/deadline handling: abrupt
owner death does not rely on core Drop running or revoke all retained mappings.
Protocol writes are nonblocking and keep checking authority/deadlines while their
pipe is full; a parked reader must not hide revocation during active output.
The helper also checks for its owned supervisor pipe disconnect before claim and
during protocol/output. Pipe checks/native preparation precede final claim; no
protocol I/O is inserted between post-claim core validation and prepared play.

The atomic claim is the playback-start linearization point: cancellation ordered
before it prevents a first play; cancellation after it retires already-started
work. This is not an OS scheduling or native-call interruptibility guarantee.
No GTK callback waits for a helper handshake. The post-loop exit guard also
revokes the current authority within its existing bounded confirmation interval.
`DecoderEnded` additionally requires exit status zero and observed
process-group quiescence. Cancellation/deadline uses SIGKILL instead of waiting
for a possibly blocked GTK clear. The unreaped leader pins the group identity;
descendant observation uses kernel getpgid/pidfds, not a `/proc/stat` PID parser.

Cleanup confirmation has a separate 500 ms bound. Failure to confirm is reported
as uncertainty, **not** successful cancellation or extra capacity: the single
slot/supervisor remains quarantined, every otherwise admissible request returns
`Busy`, and no additional worker/reaper is created. The supervisor also owns the helper's
temporary subtree, including any registry left behind by SIGKILL, and retains
the slot if deleting it fails. If the kernel later confirms cleanup, only
an explicit new request can play. No software timeout can honestly declare an
unkillable kernel task gone, or interrupt a stuck spawn/proc/filesystem syscall;
those retain the same slot and expose uncertainty without blocking GTK. After
the UI loop has exited, the narrow host exit guard gives confirmation at most
500 ms; it never runs inside a GTK callback and returns failure if cleanup is
still unknown.

`initialize_execution(root)` caches the packaged context before workers. The
decoder dispatch belongs at the start of the executable; keep other early
command-supervisor dispatches when joining changes. Do not restore the old
GUI-side `initialize_packaged` media initialization. `decoder_snapshot()` exposes
bounded observational diagnostics (512 KiB maps, 4 KiB stderr, owned PID/phase);
PIDs are never an external cancellation authority. Acknowledgements are scoped
to the operation object and cannot cancel or finish a newer owner's playback.
`Requested`/`DecoderEnded` report lifecycle, not proven audible output. Decoder
errors win over ended/prepared state; there is no VTE-beep fallback.

## Packaging and evidence boundaries

Debian/Ubuntu explicitly require `libgtk-4-media-gstreamer` and
`gstreamer1.0-plugins-good` (which requires base). Fedora's `gtk4` supplies its
media implementation; `gstreamer1-plugins-good` supplies WAV parsing/pulsesink,
and `libglvnd-gles` supplies the media paintable's dynamically opened GLES library.

Snap stages the Ubuntu media/base/good route but leaves out the Mesa GL drivers
that route pulls in (`snap/snapcraft.yaml`, `prime`). With them bundled, GTK's
display setup crashed under `snap run`. Audio needs no GL; the GUI renders
without a bundled driver, as it did before the media route. AppImage preserves the GTK media
module, ten declared GStreamer plugins and plugin scanner in their nested paths;
linuxdeploy resolves/patches their dependency RUNPATHs before attribution.
`data/audio/runtime-files.rs` is shared by packaging and startup requirements.
Startup caches only the execution root. Each owned helper initializes private
discovery with a fresh GStreamer registry. Overrides stay in that short-lived
helper: native initialization can start threads, so there is no later environment
mutation to restore them. The GUI/terminal environment never receives them.
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

`examples/audio-retirement-probe.rs` adds a separate trusted test-only native-call
interposer, never linked into Among. It stalls actual constructor/clear call
sites, crashes a helper, injects a denied kill to exercise quarantine, and releases
a delayed constructor after expiry to assert that it never reaches play. A real
GTK window/heartbeat must keep running; cancellation/shutdown/selection/drop
latency, one-slot capacity, helper/group retirement and explicit recovery are
measured alongside actual captured sound after recovery. These are injected
faults, not claims that a codec deadlock was discovered in a normal recording.
`check-audio-retirement` reuses a checked package extraction and separately bound
fault probe; the CI workflows retain its captures on both native architectures.
The cancellation cases delay the real fork (before it or only its parent return),
release actual native construction before expiry and assert no play invocation
before issuing any recovery. Cancel-only and explicit-recovery runs have separate
captures: the former requires nonempty silence, the latter actual 880 Hz output.
They cover pending grants, event-policy revocation, selection, shutdown and owner
drop without queuing a replacement; denied kill is confined to its named fixture.

## Owned effect boundary probes

`audio-effect-probe` uses privately issued managed backend/core deliveries, not
invented host authority. Its external barriers park the owner and supervisor
around real spawn, pending grant, construction and active output. Targeted and
targetless cases revoke each applicable original scope or wait for original
expiry, then require a separate explicitly issued recovery.

`audio-input-probe` forwards actual owned audio operations through test-only
malformed argument/FD injection, supervisor-pipe disconnect and abrupt owner
death. These interposers are never linked into Among. Malformed input must refuse
before native construction/play; disconnect/death cases must retire any already
started output. Recovery must use new authority. Preview remains
explicitly testable while event admission is off.

`audio-effect-capture` records an owned private null sink on separately labeled
default and copy routes. It requires observed PCM readiness and settling,
continuous process/PCM progress, at most 300 ms between observations, bounded
whole-window clock/sample skew, and both 2500 ms and 120000 fresh 48-kHz tail
frames. A recovery waits for newly observed quiet PCM before issuance and has
its own signal-analysis segment; a preceding active tone cannot satisfy it.
Dead/stalled recorders and missing terminal notices fail rather than supply
silence. Recovery/retirement snapshots and notices are saved before their
assertions and on timeout.

These probes require an isolated device-free container and genuinely usable
native media/plugin paths, or an explicitly bound packaged runtime. Package
metadata and tool existence alone do not establish native discovery. Their
results are audio-library/helper evidence, not General/VTE ingress or release
acceptance.
