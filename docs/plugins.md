# Plugins

A plugin is an executable run directly by Among Terminal as a separate process.
The host supplies no interpreter. Any language works if its installed executable
can run on the system; scripts may use a shebang and must provide their runtime.
Plugin code is never loaded into the application, and
that is the whole point: a plugin that segfaults, hangs, or misbehaves takes only
itself down, while the live terminals — which hold whatever the user is running —
stay up.

The cost is a process spawn and messaging round trips. Compiled examples also
require a build step, unlike a script that can be copied with its runtime available.

The exchange is newline-delimited JSON on the plugin's stdin and stdout. Nothing
else is shared. There is no GTK type, no widget handle, and no function pointer
crossing the boundary, so there is no ABI for either side to break.

## Live status examples

Host protocol 1.1 adds optional `meter` on a fragment: a finite fraction from 0
to 1 rendered as a compact GTK bar. Keep a readable percentage in `text`.
Bars are 50 pixels wide. Their whole track and fill share a continuously changing
hue: green at 0%, yellow at 50%, red at 100%; the track is translucent so the
filled fraction stays distinguishable. Numeric percentages remain a non-color cue.
The host's shared application header and footer use bold monospace by default,
including the clock and N/A labels. Footer text has opacity 0.7; the upper panel
and individual pane footers are not dimmed. Plugins do not select this typography.
Omit or set `meter` to null for N/A. Values outside this range are rejected.
Major 1 is unchanged; 1.0 hosts ignore the field and show only the text.

`clock` shows local 24-hour HH:MM:SS once at the right of the shared application
footer every second, demonstrating a different slot than the resource header.
It uses the process's local timezone configuration (including `TZ`), not UTC.

`system-status` shows labelled graphical CPU, MEM and DISK bars with percentages. CPU samples
the first eight aggregate `/proc/stat` counters: busy is the interval total minus
idle and iowait; guest counters are not counted twice. Startup, reset and zero
intervals show N/A. MEM is `(MemTotal - MemAvailable) / MemTotal`, not a sum that
treats all caches as unavailable. Both are rounded to the nearest percent and
sampled at most once a second. DISK targets `/` using `statvfs`: used is blocks
minus free, divided by used plus available (excluding reserved availability),
rounded upward like `df`. It is sampled at most once every five seconds.
Failed sources show N/A independently. These describe the process's proc/mount
namespace; sandbox figures need not describe the whole host.

`pane-pwd` polls each pane's last terminal-reported directory every second,
including unfocused panes. The full path is in the tooltip; long labels are
truncated. Labels contain only the path, without a `PWD:` prefix. Unknown
directories produce no content; the plugin never substitutes
the host or focused pane's directory. A known initial shell directory is available
at launch; subsequent directory changes require the shell to opt in to OSC 7.

For Bash, test this in a terminal (no shell files are modified):

```sh
among_osc7() {
    local LC_ALL=C directory uri='file://' byte encoded i
    directory=$(pwd -P && printf '.') || return
    directory=${directory%.}
    directory=${directory%$'\n'}
    for ((i=0; i<${#directory}; i++)); do
        byte=${directory:i:1}
        case "$byte" in
            [a-zA-Z0-9/._~-]) uri+=$byte ;;
            *) printf -v encoded '%%%02X' "'$byte"; uri+=$encoded ;;
        esac
    done
    printf '\033]7;%s\007' "$uri"
}
PROMPT_COMMAND="among_osc7${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
among_osc7
```

This optional helper uses Bash builtins, encoding physical-directory bytes rather
than interpreting spaces, non-ASCII names or control characters as URI syntax.
Add it to your own Bash configuration only if wanted.
Try `cd /tmp` and wait up to two seconds for the footer. Other shells require
their own prompt hook; no untested zsh/fish integration is claimed here.

## Installing

Preferences saves explicit enable/disable choices immediately to `plugins.json`
and restores them on startup. A Faulted process remains desired-enabled until
you disable it. Save failures are reported as session-only changes; see
[Preferences storage](preferences.md).

`cargo run --release` builds only the application. It does **not** build this
independent example package, replace installed plugin files or enable new ones.
To build all examples, back up installed copies and add only missing default
configuration entries, run from the repository:

```sh
sh scripts/install-plugins.sh --enable-new
```

Existing configuration fields and enabled/disabled entries are preserved. Backups
are created before replacement. All three plugins are disabled by default;
enable wanted plugins explicitly in Preferences.
Existing explicitly enabled examples stay enabled during upgrades; turn them off
in Preferences if no longer wanted. Backups
are stored under `${XDG_DATA_HOME:-$HOME/.local/share}/among/plugin-backups/`.
To update only selected binaries without changing configuration:

```sh
sh scripts/install-plugins.sh system-status clock
```

Disable/re-enable running plugins in Preferences after replacement. Newly added
configuration names load on the next host startup. A rebuilt host does not
replace an already running single-instance process: another `cargo run --release`
may activate that process. Restart the host only when it is safe to end its live
terminal sessions. Plugin updates alone do not close any terminal.

Merge plugin configuration entries with existing settings rather than replacing
them. Clock and resource plugins are optional demonstrations of status slots.

Plugins are executables in `$XDG_DATA_HOME/among/plugins` (`~/.local/share/among/plugins`),
named after the plugin:

```sh
cargo build --release --locked --manifest-path examples/plugins/Cargo.toml
plugin_dir="${XDG_DATA_HOME:-$HOME/.local/share}/among/plugins"
mkdir -p "$plugin_dir"
install -m 755 examples/plugins/target/release/clock "$plugin_dir/clock"
install -m 755 examples/plugins/target/release/system-status "$plugin_dir/system-status"
install -m 755 examples/plugins/target/release/pane-pwd "$plugin_dir/pane-pwd"
```

Then register them in `$XDG_CONFIG_HOME/among/plugins.json` (`~/.config/among/plugins.json`):

```json
{"plugins": [
  {"name": "clock", "enabled": false},
  {"name": "system-status", "enabled": false},
  {"name": "pane-pwd", "enabled": false}
]}
```

Configuration is read leniently, because one broken entry must not stop the
application from starting:

- A missing file, unparsable JSON, or a file without a `plugins` key means no
  plugins are loaded.
- A configured plugin whose file is absent remains an inspectable missing entry,
  with its desired switch retained. It can still be disabled.
- An entry without a `name` is skipped.
- Legacy names must be single filenames, not paths or managed identity strings.
  Duplicate configured names are registered once; stored unrelated data is not
  rewritten during discovery. Installed but unconfigured legacy files appear
  disabled, without adding defaults to configuration merely by inspection.

### Native inventory

Preferences → Plugins joins configured legacy entries, committed managed receipts
and runtime observations by identity. Cards show origin/version and runtime state
beside the desired enablement switch. Details shows the description (or its
absence), declared compatibility, last observed wire API and package provenance;
managed details use labelled, selectable rows rather than a single long alert.
A never-run plugin does not receive an observed-compatible badge. Equal display names retain separate
origin-scoped settings and switches; invalid selected packages remain visible.

Search matches name, description, identity and origin. Clearing search restores
the same rows rather than redirecting an action to a sorted position. Background
state changes update existing controls; they do not replace an open editor or
discard unfinished settings text. Details/settings callbacks reject a removed or
replaced package rather than silently retargeting an old action. The separate
Plugin sources section opens repository inspection with a separate explicit
Install disabled action for each compatible version; the search field filters
only installed/configured plugins. Merely inspecting metadata never installs or
enables code. Registered sources have explicit Cached, Refresh and Remove source
actions; source removal does not uninstall anything. Existing identities have a
separate Apply update action. Managed Details exposes explicit recovery/removal;
these do not modify legacy executables.

### Updating an installed example

Changing an example in this repository does not update the installed copy.
Build first, then replace the selected installed executable at the same name:

```sh
cargo build --release --locked --manifest-path examples/plugins/Cargo.toml
install -m 755 examples/plugins/target/release/clock "${XDG_DATA_HOME:-$HOME/.local/share}/among/plugins/clock"
```

Disable and re-enable `clock` in preferences to start the updated copy
without closing any terminals.
Replacing an old script with the binary does not require deleting it first.
An already-running process keeps executing its old code until restarted.

## Protocol

### Host to plugin

Every message has a `type` field.

```jsonc
// Sent once when the plugin starts.
{"type": "hello", "major": 1, "minor": 2}

// Collect data now. Answer with a status message carrying the same request_id.
{"type": "status", "request_id": 7, "scope": "pane_header:1", "context": {
  "active_workspace": 0,
  "workspace_count": 2,
  "focused_pane": 1,
  "terminals": [
    {"pane": 1, "workspace": 0, "cwd": "/home/demo/projects", "title": "bash", "running": true}
  ]
}}

// Something happened the plugin may care about.
{"type": "event", "event": {"kind": "working_directory_changed"}, "pane": 1, "detail": "/tmp"}

// The host is stopping.
{"type": "shutdown"}
```

Event kinds are `pane_opened`, `pane_closed`, `process_exited`, `working_directory_changed`,
and `workspace_activated`.

### Plugin to host

```jsonc
// Reply to hello. The major version must match the host's.
{"type": "hello", "major": 1, "minor": 0}

// The answer to a status request.
{"type": "status", "request_id": 7, "refresh_ms": 2000,
 "fragments": [
   {"icon": "git-branch-symbolic", "text": "main*", "style": "warning", "priority": 20,
    "tooltip": "Uncommitted changes present"}
 ]}
```

`refresh_ms` is how often the data changes:

- A number is the requested refresh interval; polling and transport backpressure
  can delay the next request.
- `null` means event-driven: the host asks when a terminal changes rather than on
  a timer.

That matters for expensive plugins. `git status` on a large repository should
declare a few seconds, or `null` and let terminal events drive it; either way the
host will not poll it faster than asked.

The refresh clock restarts when an answer arrives, not when the question is sent.
Without that distinction one slow plugin gets asked on every tick — for `git
state` that means forking a subprocess tens of times a second, which starves the
other plugins of response time.
Clocks are separate for each source and scope: collecting one plugin does not
mark a deferred sibling as collected. Scope scheduling rotates so continuously
due content cannot always take capacity ahead of later panes and summaries.

## Scopes

Where content is shown. A plugin is told which scope it is answering.

| Scope                  | Appears                                    |
|------------------------|--------------------------------------------|
| `app_header`           | the window's top strip                     |
| `app_footer`           | the window's bottom strip                  |
| `workspace_header`     | above the active workspace's panes         |
| `pane_header:<id>`     | above that pane's terminal                 |
| `pane_footer:<id>`     | below that pane's terminal                 |
| `workspace_summary:<n>`| beside workspace `n`'s name in the sidebar |

The sidebar summary takes the first fragment of the response. Several plugins may
each contribute one, and all of them are shown — one plugin never replaces
another's content, and a plugin supplying more content never causes another's to
be dropped.

A pane header belongs to that pane alone: content supplied for one pane does not
appear in another pane, nor in the workspace as a whole.

## Fragments

A fragment is pure data. The host decides how to draw it, which is why a plugin
cannot break the layout by producing something oversized.

```jsonc
{
  "text": "main",          // optional; may be omitted if an icon is given
  "icon": "folder-symbolic", // optional GTK icon name
  "style": "success",      // success | warning | error | accent | dim
  "tooltip": "Clean tree", // optional
  "priority": 20           // lower sorts first within a slot
}
```

`style` is a closed set, and these are all of its names:

| Name | Conveys |
| --- | --- |
| `success` | the thing reported is in good order |
| `warning` | it needs attention but nothing is broken |
| `error` | it is broken |
| `accent` | it is worth looking at, with no judgement |
| `dim` | it is present but unimportant |

The first four are coloured from the platform's own semantic colours and are kept
distinct from one another. `dim` is not a sixth colour: it is the ordinary text
at reduced opacity, so quietness is shown by weight rather than by another hue.

Omit `style` entirely for ordinary text — that is the default appearance.

A name outside this set, or no name at all, draws the fragment in the default
appearance rather than failing the message. A plugin is never disconnected over a
style it guessed wrong. An icon the theme
does not have is dropped rather than drawn as a broken placeholder, and long text
is elided.

Order within a slot is by `priority`, then by plugin name, so re-sending the same
data never reshuffles a slot.

## Versioning

The protocol version is negotiated at start-up and is independent of the
application's version.

- The **major** version must match exactly. A plugin whose major differs is
  reported as incompatible and its data is ignored — the host does not guess.
- Legacy **minor** versions may be lower or higher: the legacy profile checks
  major equality only. Managed admission separately checks its API and features.

### Static managed metadata and settings

Managed packages use a separate static `among-plugin.json`, schema version 1.
The [complete vocabulary example](../examples/managed-manifest.json) is parsed
by the production parser in tests. It is data, not a ready-built plugin package.
Static metadata and local settings can be inspected without executing code.
API 1.2 adds the distinct managed profile below; legacy executables still use
`hello` and the existing major-1 messages. Managed packages never fall back to
legacy execution. Notification production, notification UI and delivery features
remain unsupported and are not advertised.

Manifest `id` is lowercase ASCII letters/digits/hyphens, at most 64 bytes.
`version`, `name` and `description` are bounded display metadata, not authority.
`entrypoint` is a relative executable file, never a shell command. Entrypoints
and resources cannot use absolute paths, `..`, symlinks, hardlinks or special
files. The per-user package directory/files must be owned and not writable by
others. Inspection runs no `--version` probe, hook or downloaded validator.
JSON is limited to 256 KiB and depth 32; duplicate keys, feature IDs and channel
IDs are rejected. There are at most 32 declared features/channels each.

Icons are optional `theme:<icon-name>` or contained static PNG files. PNGs have
a 1-MiB encoded and 2048-by-2048 decoded limit; animated/compressed ancillary
metadata is refused. Invalid optional artwork uses a host fallback. An escaping
resource path is a package refusal, not a fallback to an outside file.

Settings declare their own positive `version`, `apply` (`live` or `restart`) and
up to 64 fields with stable `key`, `label`, `kind`, optional typed `default` and
applicable bounds. Supported kinds are boolean, integer, finite number, string
and enum. Enum choices have distinct `value` and `label`. Fields are required
unless `required` is false. Required unsupported kinds make the schema
incompatible; optional unsupported kinds remain unavailable. A secret field
never becomes an ordinary or masked string control. No secret storage is
supported. Defaults are displayed in memory, not written merely by inspection.

The native Settings action validates an explicit Save, even while the plugin is
disabled. Effective declared values fit 32 KiB; the complete local envelope fits
256 KiB. Unknown local values survive edits but are not delivered as declared
settings. A different stored settings version requires an explicit migration
decision; inspection never rewrites it. Local saving and runtime application
are distinct: the editor does not report a saved value as applied to a process.
Diagnostics use host-owned reason text, not submitted settings values.

Optional fields without a stored value or default stay absent until edited;
an untouched blank optional numeric field does not block saving another field.
Invalid explicit edits are refused as a whole, with the declared key/type/bounds
reason but no submitted value in the diagnostic. Unsupported secret fields never
acquire ordinary editors or materialized defaults.

Validated Save outcomes distinguish durable local saving, session-only acceptance
after a storage failure, and a written replacement whose durability could not be
confirmed. Session-only accepted values remain available in the current host and
can be delivered to the current process when their apply mode permits it. A later
managed preference edit retries pending identity-scoped settings/switch/review
changes together, merging current unknown data rather than erasing it. Restarting
the application before successful persistence can lose session-only changes.

Managed installation selection is separate from legacy executables. The
application reads only its committed `among/managed/installed.json` record in
the XDG data directory, not arbitrary discovered manifests or staging files.
It retains invalid/missing selected packages as unavailable entries. A receipt
binds GitHub's repository ID, owner/name, repository-local ID, exact revision,
manifest/package SHA-256, release/asset IDs and immutable generation. A matching
hash establishes content consistency, not an independently verified publisher.
Identity is `github:<repository-id>/<local-id>`; a bare filename/display name
cannot inherit another origin's settings or switch. Installer receipts also carry
a fresh `installation` token. Explicit managed enable/review saves that token with
consent; retained enabled settings for a different/missing token cannot start a new
installation. Pre-installer receipts without tokens keep their previous startup
semantics. New installs do not rewrite settings merely to force their switch off.
New receipts also bind the numeric GitHub publisher ID, exact catalogue revision
and catalogue SHA-256. Access review for these receipts binds numeric repository
and publisher IDs: renaming keeps authority, but transfer or deleting/recreating a
name cannot inherit consent. Old label-only receipts keep startup compatibility;
upgrading their unconfirmed publisher boundary requires renewed review.
Local state lives separately in `managed-plugins.json` in the XDG config directory.
Retained preferences alone are not evidence of installation. Repository browsing,
download, update and uninstall are separate installer operations, not implemented
by static inspection. Existing manifestless discovery/configuration and all three
bundled examples keep their legacy identities and enablement.

### Managed admission, context and settings (API 1.2)

The first enable opens an access review identifying the authoritative repository
and required/optional API features. Optional access starts off. Cancel does not
write grants or start code. Required access must be accepted; unsupported required
features refuse startup before execution. Retained review is scoped to publisher,
repository and declared grants. A publisher change or newly required access needs
renewed review. Changing grants requires stopping/reaping the current generation.
These controls limit host-mediated exposure, not the process's OS authority.

The host rechecks the selected manifest digest and contained executable and pins
open package/executable descriptors through spawn. It clears the child environment,
sets `PATH=/usr/local/bin:/usr/bin:/bin`, `LANG=C.UTF-8`, and copies only `HOME` and
`XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_CACHE_HOME`, `XDG_RUNTIME_DIR` when present.
Tokens, display/session-bus variables and other parent environment values are not
implicitly inherited. This is not protection against the plugin independently
reading files or contacting services as the same user.

The host starts with:

```json
{"type":"managed_hello","major":1,"minor":2,"identity":"github:12/sample","session":"session-id","generation":"process-id","features":["context.terminals","settings","status"]}
```

Reply with `managed_hello`, matching identity, session, generation and API 1.2,
and a feature subset of the offered grants that still includes every manifest
requirement. Only then does Starting become Running. The deadline is five seconds;
pre-admission output has no authority. Optional features can be omitted by the
plugin. Each new process gets a fresh generation; each host session gets a fresh
session ID. Old replies cannot establish admission or acknowledge new settings.

| Feature | Actual host-mediated exposure |
| --- | --- |
| `status` | Answer status requests with validated legacy fragment data |
| `settings` | Receive declared, validated configuration snapshots |
| `context.terminals` | Scoped terminal/workspace references and enumeration |
| `context.cwd` | Known working directory, or null |
| `context.title` | Known terminal title, or null |
| `context.process` | Running state and process-exit events |
| `events.terminal` | Subscribe to terminal/workspace lifecycle events |

The last four require `context.terminals`. Missing grants omit fields rather than
fill them with fake values. Without terminal context, status is application-only
and its context contains only the session. Events require both terminal context
and `events.terminal`; process-exit events additionally require `context.process`.
Working-directory events carry a path only with `context.cwd`. Lifecycle events
carry canonical string pane/workspace references, not mutable list positions.

Managed status uses an explicit target rather than a legacy index scope:

```json
{"type":"status","session":"session-id","generation":"process-id","request_id":7,"scope":{"slot":"workspace_summary","target":{"kind":"workspace","reference":"10"}},"context":{"session":"session-id","terminals":[{"pane":"1","workspace":"10"}],"next_cursor":null}}
```

Reply with `type`, `session`, `generation`, `request_id`, `fragments` and optional
`refresh_ms`. Application targets use `{"kind":"application"}`; pane targets use
`{"kind":"pane","reference":"1"}`. Slot names otherwise match the existing scope
table without colon suffixes. References are canonical decimal strings, never JSON
numbers, leading-zero aliases or display names. Workspace references survive the
birth pane closing and workspace reorder; closed targets expire. They are not
saved as replay authority in session files. Responses are resolved against the
current target, so a delayed sidebar reply cannot attach to a reused list index.

Context pages contain at most 32 terminals and roughly 28 KiB of terminal data.
When `next_cursor` is non-null, send `enumerate` with session, generation, your
request ID, the same scoped target and that string cursor. The host replies with
`context_page`, echoing those identifiers and carrying `context`. Only one
enumeration request may await host enqueue at a time; page requests need terminal
context and valid live targets. Pages reflect current state, not a frozen snapshot;
terminal creation/removal between pages can change the enumeration. Authorized
text over 4096 UTF-8 bytes or 8192 JSON bytes is null with its field name in
`unavailable`; paths are never silently truncated. Unauthorized fields are absent.

Settings use `configure`/`configured`:

```json
{"type":"configure","session":"session-id","generation":"process-id","revision":3,"version":1,"values":{"label":"current"}}
```

Reply with matching session, generation, revision and `outcome`: `applied`,
`rejected` or `restart_required`. Optional `reason` is one of
`invalid_configuration`, `unsupported_version`, `unavailable`, `internal_error`.
Free-form acknowledgement text is not displayed or logged. Unknown stored keys
are preserved locally but never included in declared `values`.

The editor distinguishes Saved locally, awaiting acknowledgement, Applied,
Rejected and Restart required. A configuration unanswered for five seconds is
reported unavailable, not applied; its eventual late reply cannot change that
state. A manifest's `apply: restart` saves without sending live changes, and the
next process receives them. Superseded revisions and stopped generations cannot
mark a newer revision applied. Managed `notify` remains a generic, payload-omitted
diagnostic; it never triggers notification routing or external delivery.

When a running plugin needs restart, its form offers **Restart plugin…** with
explicit confirmation. Cancel keeps the current process and accepted settings.
Confirmation restarts only that plugin, preserving the desired switch and live
terminals; it cannot implicitly enable a disabled plugin. The callback checks the
current receipt and process generation, so a dialog from before replacement or a
process restart cannot restart a different generation.

A current `applied` acknowledgement invalidates that plugin's cached status and
workspace summaries, including event-driven content. Pre-configuration status
replies keep their transport credit until consumed or stopped, but cannot restore
the old content after application; collection then requests the current values.

## Failure handling

Plugins are untrusted input. Everything below is discarded and the application
continues:

- A line that is not valid JSON, or does not match the schema.
- A message of a type the host does not know.
- A value out of range: `priority` outside −1000…1000, `text` longer than 512
  characters, or `refresh_ms` above an hour.
- More than 64 fragments in one response.
- A fragment with neither text nor an icon.

Stdout frames are limited to 64 KiB including the newline, with at most 32 JSON
nesting levels. The reader never allocates an unlimited unterminated line. Each
plugin's stdout queue holds at most 16 frames (at most 1 MiB); exceeding either
the frame or queue limit faults that plugin instead of blocking the interface.
Polling rotates its starting plugin and drains at most 16 frames per plugin,
256 globally and five milliseconds of frame work per tick. A noisy source does
not always run before every other source. Pending result/notification batches
are also capped at 256 entries each.

Stdin uses a separate bounded 16-frame/1-MiB budget, including a partially written
frame, with a two-second enqueue-to-write deadline. Serialization stops at the
64-KiB frame limit too. Spawn, pipe reads/writes, termination and reaping run in
one cancellable worker per supervised process, not on the GUI thread. The host
supervises at most 32 processes, including generations still stopping. A stopped
generation cannot be replaced until its worker has actually reaped it.
If the last window closes and the application is reactivated while old plugins
are stopping, the replacement window opens immediately but its plugins show
Waiting until all retiring hosts finish reaping. This preserves the same
application-wide process budget across window replacement.

Starting is separate from Running: a compatible handshake is required within
five seconds. Legacy plugins allow at most 16 outstanding statuses, one per scope.
Managed statuses and configuration revisions share 15 reply credits, including
superseded/expired revisions and revoked targets whose replies have not arrived;
one output slot is reserved for the single enumeration request. Reply consumption
or generation shutdown releases credit. A full input
queue defers status collection without faulting the plugin or advancing its
collection clock; accepted frames still have the write deadline. Other traffic
that exceeds the input budget faults the plugin rather than growing a queue.
Unmatched, duplicate and
previous-generation replies cannot update slots. Both faults and disablement
withdraw the last contribution even when there are no new results.

A plugin that crashes, exits, or does not answer within the timeout loses its
content immediately. Stopping sends TERM to its supervised process group,
escalates to KILL after 500 ms and reaps off-loop. It remains Stopping until exit
is observed. At most three automatic restarts follow a fault, after 2, 4 and 8
seconds; an explicit off/on cycle resets that allowance. Incompatible or missing
executables are not automatically retried. The application and every terminal
stay usable throughout. This is supervision, not an OS sandbox: descendants that
deliberately escape the owned group retain the user's ordinary process authority.

Standard error is discarded by the host. Use a protocol `error` response for a
diagnostic, or exit nonzero for an unrecoverable failure so the host notices it;
printing to stderr alone does not reach the user.

Because the pipe to a plugin is separate from the interface, a plugin stuck in a
long computation cannot freeze the window: the host reads its output off the main
loop and applies a timeout instead of waiting inline.

## Writing one

### Third-party plugin quick start

Plugins run as your user, not in a security sandbox. Install only code you trust.
They can use any language; the host launches the file directly, so scripts need
a shebang, executable permissions and an installed interpreter. Only protocol
JSON lines belong on stdout, and every reply must be flushed.

Create an independent Cargo package with a JSON dependency:

```sh
cargo new --bin my-status
cd my-status
cargo add serde_json@1
```

Replace `src/main.rs` with:

```rust
use std::io::{self, BufRead, Write};
use serde_json::{json, Value};

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { continue };
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        let kind = message["type"].as_str().unwrap_or_default();
        let reply = match kind {
            // The host is going away; stop rather than block on stdin.
            "shutdown" => break,
            "hello" => json!({"type": "hello", "major": 1, "minor": 1}),
            "status" => {
                // Only the app header carries fragments; every other scope
                // gets an empty list, which the host renders as nothing.
                let fragments = if message["scope"] == "app_header" {
                    json!([{"text": "Hello", "priority": 10}])
                } else {
                    json!([])
                };
                json!({
                    "type": "status",
                    "request_id": message["request_id"].as_i64().unwrap_or_default(),
                    "refresh_ms": 2000,
                    "fragments": fragments,
                })
            }
            _ => continue,
        };
        // Every reply must be flushed: the host reads line by line and a
        // buffered reply is indistinguishable from a hung plugin.
        writeln!(stdout, "{reply}")?;
        stdout.flush()?;
    }
    Ok(())
}
```

Build it and install it under the same name you will register:

```sh
plugin_dir="${XDG_DATA_HOME:-$HOME/.local/share}/among/plugins"
mkdir -p "$plugin_dir"
cargo build --release --locked
install -m 755 target/release/my-status "$plugin_dir/my-status"
```

To retain an explicit choice, add `{"name": "my-status", "enabled": false}` to the `plugins` array in
`${XDG_CONFIG_HOME:-$HOME/.config}/among/plugins.json`, preserving other entries
and fields. If the file does not exist, create it with:

```json
{"plugins": [{"name": "my-status", "enabled": false}]}
```

The configuration name is the installed filename, not a path or display title.
At startup the host retains configured names even when their files are missing,
and discovers unconfigured executable files in the legacy plugin directory as
disabled entries. Copying a new file does not trigger live legacy discovery.
Restart Among Terminal when it is safe to close its terminal sessions, then open
Settings → Plugins and enable `my-status`; `Hello` will
appear in the application header. A protocol or launch failure is shown as
Faulted rather than Running. For an already registered plugin update, disable
it, replace its file, then re-enable it; no host restart is needed.

For compiled Rust plugins, start with the independent package below; for other
languages, use the same JSON-lines protocol documented above. There is no GTK
dependency or application SDK requirement.

The independent [example package](../examples/plugins/Cargo.toml) contains
standalone binaries and [shared stdio plumbing](../examples/plugins/src/protocol.rs).
It does not depend on or link the host. Within that package, a binary can use:

```rust
use among_example_plugins::protocol::{run, Status};
use serde_json::json;

fn main() -> std::process::ExitCode {
    run(|scope, _context| Status {
        refresh_ms: Some(2000),
        fragments: if scope == "app_header" {
            vec![json!({"text": "Hello", "priority": 10})]
        } else {
            Vec::new()
        },
    })
}
```

The loop handles the version 1.0 handshake, request IDs, malformed JSON, events,
flushing, EOF and shutdown. For a third-party package, implement that same wire
contract or reuse the independent example code; no application library is needed.

`pane-pwd` reports each pane's last known directory, including its actual initial
shell directory. Later changes require OSC 7; an unknown directory emits no content.

Test the binaries and their real stdio replies with:

```sh
cargo test --locked --manifest-path examples/plugins/Cargo.toml
```

## Repository package metadata — implementation in progress

Preferences → Plugins → Plugin sources → Inspect repository opens a
public-GitHub catalogue preview. Enter `owner/repository` or its public HTTPS
GitHub URL, then use the adjacent Inspect button or press Enter. Loading exposes
a Cancel request action; empty/error/cancelled states do not show a blank result
panel. Editing the address hides the previous preview without sending a request.
After inspecting a catalogue, each compatible version has an explicit **Install
disabled** action for new identities, or **Apply update…** for a currently installed
identity. Ordinary installation still refuses replacement. Updates have separate
confirmation disclosing the exact version, access and target-only restart. Source
inspection alone cannot install, update, enable or grant execution access. This is
not full installer acceptance: integrated notification providers, separate settings
deletion, independent security review, supported-platform observations and CI gates
remain separate from the local managed-process workflows.

**Add source** explicitly saves the inspected numeric repository/publisher identity
and validated snapshot. Inspection or address edits alone never register it.
Registered sources reopen from bounded local storage without network access.
**Cached** displays the actual retrieval timestamp, whether the snapshot is at least
24 hours old, and that it is not an online check. A future timestamp/clock rollback
is stale/uncertain, not fresh. No cache is a promise of network availability.

**Refresh** requests the registered numeric GitHub repository ID, never the old
owner/name alias. A rename retains identity. A changed numeric publisher requires
the explicit **Approve publisher change** action; refresh does not silently approve
it or grant installed code any access. Reusing an owner/repository spelling with a
different numeric repository ID is a separate explicit addition, not an update.
Failed/rate-limited/cancelled refresh keeps the committed cache and installed
selection/switch/settings unchanged. **Remove source** removes discovery and its
cache only; it neither disables nor uninstalls a selected package.

Registrations and exact raw catalogue bytes live in the owned data directory
`among/plugin-sources/sources.json`, separate from managed selections and config.
The total metadata storage budget is 16 MiB: the committed document is capped at
8 MiB, reserving the other half for its flushed atomic replacement. There are at
most 32 repositories and four snapshots per repository. Old snapshots can be evicted for cache quota without deleting
registrations, settings or installed generations. A refreshed duplicate revision/
digest replaces its retrieval observation rather than consuming another slot.
Publisher reapproval drops the previous publisher's cached snapshots.

Source edits use no-follow owned storage, a bounded worker/shared two-credit
budget, advisory serialization, flushed atomic replacement and an exact previous
document/root-inode check. Written-but-unconfirmed durability is reported separately.
Only safe files in the reserved `sources-<64-hex>` temporary namespace are recovered;
links/unsafe entries refuse cleanup, unrelated files remain untouched. Directory
enumeration is capped at 256 entries; one active atomic temporary is bounded by
the same 8-MiB document cap. Each registration has a fresh token and edit revision;
late refresh/remove callbacks cannot resurrect a removed/re-added source or target
another source with the same label. Cached metadata is revalidated before display
and compatibility checks run off the GUI thread without probes.

The metadata client uses real HTTPS, resolves repository identity, pins catalogue
content to a revision and accepts only constrained public-GitHub redirects.
It has 5-second connect, 10-second idle-read and 60-second operation limits,
bounded streamed responses, cancellation and at most two retained network workers.
It neither builds nor executes downloaded code.

The preflight parser defines `among-plugins.json` schema 1. A root version
envelope normalizes to one plugin/version; a multi-plugin root has
`schema_version: 1` and a `plugins` list of independent `id`/`versions` entries.
Each version embeds the existing shared plugin `manifest`, an exact 40-hex commit
`revision`, `manifest_sha256`, numeric GitHub `release_id` and `artifacts`.
Unknown fields, duplicate keys/IDs/version labels and executable hooks are refused.

Artifacts declare `os: linux`, `architecture: amd64` or `arm64`, a `runtime`
(`native` with `libc` and numeric `minimum`, or `script` with an explicit installed
`/usr/bin/<name>` executable), `dependencies`, `format: tar.gz`, numeric `asset_id`,
exact `size`/`sha256` and `members`. Each regular-file member declares `path`,
`size`, `sha256` and `executable`; the shared manifest and executable entrypoint
must be present. This metadata selects ready-built packages, not DEB/RPM installers
or a build command. Digest consistency is not independent publisher authentication.

Preflight bounds catalogue bytes to 1 MiB, 32 plugins, 16 versions per plugin,
8 artifacts per version and 16 dependencies per artifact. Shared manifests retain
their 256-KiB limit. Package declarations are bounded to 64 MiB downloaded,
256 MiB expanded, 2,048 regular files and 64 MiB per file. Paths use the shared
256-byte/16-component relative-path rules; duplicate or conflicting file paths,
absolute paths, empty/dot/parent components and inconsistent manifest digests fail.
Platform selection never guesses from filenames or falls back to another CPU.
An installed runtime file does not prove its minimum version: an unmeasured
minimum is reported unavailable instead of executing a probe.

The install/update consumer requests the exact numeric repository/release, checks
selected asset membership/size and any supplied GitHub digest, then streams that
exact numeric asset. It never refetches mutable latest. Signed CDN redirects must
use GitHub's release-asset hosts and the selected numeric repository path. Tokens
are not logged or stored in receipts. Size and SHA-256 must match selected metadata.
The same connect/idle/operation deadlines and two worker credits cover inspection
and package mutations/recovery; overflow is refused, not put in an unbounded queue.

Extraction uses fresh owned descriptor-relative staging. Regular files must match
the declared size, executable mode and digest. Links, special/extended headers,
duplicate/unsafe paths, unexpected members, trailing compressed streams and invalid
gzip trailers are refused. The packaged manifest must match both its exact digest
and the shared parsed preview. Shared icon inspection stays inert. Extraction has
a ten-second read deadline, checks cancellation per member/128 KiB and bounds tar
headers/padding in addition to declared expanded bytes.

The store bounds selection to 128 identities/256 KiB, package/staging bytes to
2 GiB total/512 MiB staging, directory depth to 24, per-directory entries to 131,072
and a measured traversal to 1,048,576 entries. Normal staging cleanup is bounded
to 65,536 entries/five seconds; unsafe entries leave a diagnostic/deferred staging
directory, never a followed link. Advisory serialization has cancellable off-loop
waiting bounded to five seconds. It serializes cooperating package writers,
including updates/removal and recovery. It does not protect against arbitrary same-user
filesystem modification and is not an OS sandbox.

Complete flushed payloads move to fresh generations with no-replace rename before
an atomic `installed.json` selection rename. Pre-selection failure leaves at most
an inert unselected generation; it does not report Installed. A changed selection
snapshot/root or published-generation inode is refused rather than overwriting an
observed external edit or selecting a hidden generation. Final
directory-confirmation failure reports written-but-uncertain, not durable success.
Cancellation is accepted only before commitment starts; a committed operation is
not reported rolled back. Its completion reconciles disabled live inventory even
if the repository view has closed. Selected files/settings/legacy executables are
never replaced in place by package installation/update.

Install jobs have identity/operation-generation-bound coalesced progress and retain
their per-identity slot until the worker actually stops, including cancellation.
Application-wide completion dispatch targets the current live window's host, not
a retiring host captured by an old view. It cannot implicitly start the new plugin.
An update preserves the installation token, rereads the exact expected receipt
under store authority and retains the previous verified receipt/generation in the
same selection document. Live reconciliation uses the host's current desired
choice, never the download's earlier snapshot: a later accepted disable wins.
Selected and actually running package versions are observed separately while the
old supervisor stops. Numeric publisher/access changes and incompatible local
settings can refuse activation; no downloaded migration code runs. Details →
**Recover previous…** explicitly reselects the retained verified generation with
normal access/settings/API admission, without claiming rollback of external effects.

Details → **Uninstall…** is separate from source deregistration. Confirmation
invalidates the identity's pending package worker, blocks stale enable/restart
controls, withdraws contributions and stops its supervised process. Removal waits
up to five seconds for actual stop acknowledgement, then commits absence and a new
store revision. Only that committed result removes the Installed entry. Failed
stop/commit remains partial, and uncertain directory durability is not ordinary
success. A reinstall uses a new token and stays disabled despite retained enabled
preferences. Unknown/local values and grants remain stored; separately consented
settings deletion is not yet implemented.

New generations contain reserved `.among-receipt.json` and `.among-lease` files;
packages cannot declare `.among-*` members. The receipt seal must agree with the
selected record. Inventory snapshots/resources hold shared advisory leases, and
the actual supervisor command retains its lease through kill/reap even after host
teardown. Cleanup needs an exclusive lease and never removes selected/previous
generations. Only owned sealed unreferenced generations and the reserved owned
staging/selection-temporary namespaces are recovered. Unsealed legacy directories,
unrelated files, external executables and local settings are not garbage-collected.
Unsafe entries defer cleanup rather than following links.

Startup schedules bounded recovery off the GUI thread, and later package operations
retry it under the same store lock. Recovery reads complete selection records; an
orphan is never inferred installed. It caps generation enumeration at 4,096 and
uses the existing bounded tree cleanup. A per-operation 256-KiB reservation covers
the added receipt/lease overhead in admission quotas. Limits are engineering
constants, not user-facing options. The full update/uninstall/lease task marks still
require their adversarial and combined-application coverage, not just these paths.
