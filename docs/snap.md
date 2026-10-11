# Snap distribution preparation

An exploratory amd64 Snap 0.1.0 has been built and inspected locally in an Ubuntu
24.04 Docker builder under `/tmp/opencode`, without host package installation.
It has been installed only in a disposable Ubuntu 24.04 KVM guest, with explicit
authorization; its first launch exposed missing GSettings schemas. The corrected
candidate passes guest shell/tool/plugin, focus and live-status checks, plus a
user-modified VTE/GDB experiment; remaining gates prevent release approval. Snapcraft and this
package are not installed on the development host. Host system provisioning or
installation still requires separate authorization. The initial experiment targeted
core24/amd64/classic. Do not use a Debian-built executable as proof of Ubuntu ABI
compatibility, and do not advertise arm64 or sandbox runtime support.

The initial [recipe](../snap/snapcraft.yaml) uses the candidate name `among` to
retain the command name; registration remains owner-only and may fail if the
name is unavailable. Its grade is deliberately `devel`, so this candidate cannot
be promoted to stable. The Rust plugin builds both package paths with locked
dependencies and toolchain 1.98.1; package versions must match before injection.
Examples are bundled under `usr/share/among/plugins` and never auto-installed into
the user's home. Native payload source/license collection and installed runtime
validation are unfinished; the YAML is not evidence of a successful build.
The actual candidate build is recorded in the release change's `verification.md`;
classic lint passed; the desktop icon-lookup warning was fixed and the candidate
rebuilt. Five unused-library warnings remain under investigation.
Its ELF loader/RUNPATH and compressed metadata were inspected; these checks do
not themselves establish installed desktop/host-shell behavior or source compliance.
Separate installed checks observed Bash startup, a typed host tool in a synthetic
project, preserved caller environment and uncontaminated child library maps,
external plugin content, pointer/survivor focus, moves/background PWD, narrow
footers and plugin-crash containment. This coverage is guest X11 only; the full
runtime/source audit is not yet complete.
The early-loader candidate passed actual SVG decoding and screenshot review:
all three symbolic header icons render in normal and narrow guest windows.
Installed F11, keyboard/pointer resize, zoom/restore and session regressions also
pass. CJK glyph coverage remains unproven despite passing UTF-8 path/tooltip
integrity checks; full release readiness is not established by these UI checks.

Current package construction belongs only to GitHub release CI, using native
amd64/arm64 runners and the preserved source/recipe inputs. Inspect the resulting
artifact and run its library/classic linters. Only then install the exact checked
artifact into a dedicated test
machine with `snap install --dangerous --classic ./among_<version>_amd64.snap`.
That command changes system state: do not run it on a personal machine as part
of routine checks. Local installation occurred only in the separate test VM;
no publication command has been executed.

Later private hosted rehearsals built and installed the changed core24/devel
recipe natively on amd64 and arm64. The locked Rust version helper reads both
product/plugin manifests and is compiled outside the staged payload; internal
tooling leakage checks passed. Native Rust light/dark menu, focus/status and
Sessions drivers passed on both installed architectures. These observations do
not approve a stable-grade identity, Store/classic registration or complete native
source/license delivery.

Separate clean-runtime hosted checks also passed on amd64 and arm64 for the
devel recipe. Native Rust drivers and the clipboard helper are transferred as
internal artifacts, bound to source, version, target, architecture and SHA-256.
They are not included in the user-facing Snap. The runtime has no Cargo, Rust,
C/C++ compiler, development headers or project Python GI/YAML bindings; it
validates transferred hashes before installing the exact Snap, validates checker
records before running acceptance, and rejects wrong source and altered bytes.
These digests protect the trusted CI transfer; they are not independent signatures.

`.github/workflows/snap-build.yml` uses
`packaging/ci/snap-runtime/Dockerfile` and `check-installed.sh` for this separate
runtime. Systemd, securityfs and AppArmor preparation require a privileged
container only on a disposable hosted runner. The checks run as an unprivileged
runtime user with no source checkout mounted. Do not reproduce installed
acceptance on a personal machine. Prior intermittent pending-consent close
timeouts remain unexplained; a successful run does not prove they are resolved.

For reproduction with separately supplied native drivers on an owned display:

```sh
among-tools ci-x11 among-tools check-context-menu-snap /absolute/check-clipboard light --github-hosted
among-tools ci-x11 among-tools check-context-menu-snap /absolute/check-clipboard dark --github-hosted
```

The hosted flag requires both original GitHub-hosted markers. In a freshly created
KVM/QEMU guest omit it; the virtualization guard still applies. The clipboard
helper is the prebuilt application example, not Python GI or a shipped checker.

## Confinement and approval

Arbitrary host shells, developer tools, project files and external executables
are core terminal behavior. Classic confinement provides unsandboxed host access;
it is not a security boundary. Declaring `confinement: classic` is sufficient for
local crafting, but distributing through the Snap Store requires Store-team
approval. The publisher must register an available name and submit the classic
review request; ownership and approval are not granted by this repository.

The official guide links the Store review process. Explain why narrower
interfaces cannot provide the intended host terminal behavior and follow the
reviewer's publisher/account-verification requests. Neither approval nor name
availability has been checked for Among Terminal.

## Build and runtime gates

core24 is based on Ubuntu 24.04 LTS. Build in that environment with the selected
pinned toolchain, stage compatible native libraries, run the classic/library
linters and inspect actual ELF interpreter/RPATH resolution. Core24 classic
binaries must use bundled or base libraries, not silently depend on the host's
different ABI. Keep application-private loader settings out of spawned host
shells. Installed checks must exercise host tools, startup files, project access,
focus/panes/background jobs and plugins in private test XDG directories.

Native library source/notice and replacement/debugging obligations are release
gates described in [licensing.md](licensing.md). Local Cargo notices do not satisfy
an audit of the actual Snap payload.

## Disposable VM checks

After installing the exact SHA-256-checked candidate in a dedicated KVM/QEMU
Ubuntu 24.04 test guest, install Xvfb, Openbox, xdotool, dbus-x11 and x11-utils
there, not on a personal host. Create `/tmp/opencode` writable by the test user.
Copy the project check scripts into the guest, then run:

```sh
among-tools vm-x11 among-tools check-snap-runtime
```

Add `--github-hosted` to `check-snap-runtime` when running on a GitHub-hosted
runner; omit it inside a freshly created guest, where the virtualization guard
still applies. Modified-VTE replacement and the GDB stop are not ported yet:
the command refuses `--replace-vte` rather than silently skipping that
acceptance. The replacement and debug procedure is recorded in
`docs/source-delivery.md`; it is not automated and no acceptance route offers it
today. Treat that procedure as a
supported reproduction command.

The harness checks virtualization before creating a headless guest display and
uses an allowlisted environment. The runtime check launches `/snap/bin/among`,
inspects the actual executable/library maps, types a real host-tool command in
the terminal and checks startup files, project access, caller environment and a
user-installed external plugin. Temporary HOME/XDG directories contain synthetic
fixtures only. This is native X11/software-display evidence, not Wayland or GPU
coverage. A VM preflight check is an accidental-use guard, not proof that an
arbitrary user's VM contains no personal data: use a freshly created guest.

The replacement variant modifies a private copy of native VTE by adding a
user-built probe dependency. It uses the core24 loader's per-invocation
`--library-path`, observes the modified library/probe in the live application's
maps and verifies that the spawned host tool did not load those libraries. It
then requests a real GDB breakpoint in the probe. The mounted Snap itself is
never modified. These scripts describe the checks; recorded executed evidence,
not their existence, determines whether a candidate passes.

Snap stage-packages do not run Debian postinst scripts. The recipe compiles
GSettings XML during staging, leaving Snapcraft's automatic prime/ELF patching
intact. Earliest application startup caches the bundled GLib schema source and
restores the caller's schema environment before GTK/workers and terminal/plugin
spawn; no application-private `LD_LIBRARY_PATH` is exported into host shells.
Icon discovery uses the display's GTK IconTheme search path instead of exporting
XDG_DATA_DIRS. SVG rendering also needs librsvg's GdkPixbuf loader: the recipe
generates a loader cache targeting the installed `/snap/among/current` mount and
initializes it through GdkPixbuf's module API. Installed checks require actual
SVG decoding plus visual screenshot review; an icon-name lookup alone previously
passed while buttons still showed missing-image placeholders. Early initialization
passed installed decoding and normal/narrow screenshot review in the test VM.
SVG recognition also requires shared MIME data. The recipe generates mime.cache
and application startup prepends that directory through GIO's MIME-directory API,
not through exported environment variables. The isolated test deliberately keeps
the caller's XDG_DATA_DIRS independent of the payload and checks its preservation
in a real spawned host command.
MIME and loader initialization runs before constructing AdwApplication: GTK
caches SVG support during initialization, so registering a loader only when
building the window can leave placeholders even when direct SVG decoding passes.
Missing compiled MIME data or loader initialization errors stop startup visibly.

## Official sources checked on 2026-10-07

- [Classic confinement](https://ubuntu.com/docs/snapcraft/9.1/explanation/classic-confinement/index.html.md)
- [Enable classic confinement and request review](https://ubuntu.com/docs/snapcraft/9.1/how-to/crafting/enable-classic-confinement/index.html.md)
- [Bases](https://ubuntu.com/docs/snapcraft/9.1/reference/bases/index.html.md)
- [Rust plugin and pinned rust-channel](https://ubuntu.com/docs/snapcraft/9.1/common/craft-parts/reference/plugins/rust_plugin/index.html.md)

These are accessible official Markdown pages, selected from the current Snapcraft
`llms.txt`, not the previously login-gated legacy URL. Source probes:
`documentation.ubuntu.com/llms.txt` returned 200/plain text; its `llms-full.txt`
returned 404. The initially guessed page `.md` returned 404. The Snapcraft-specific
index redirected to Ubuntu's current index, whose links supplied these pages.
At `ubuntu.com`, both root llms files returned 200/plain text, and the three
confinement/base page probes returned 200/Markdown. No HTML fallback was needed.
