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
- A plugin named in the configuration that is not installed is reported and
  skipped; the rest still load.
- An entry without a `name` is skipped.

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
{"type": "hello", "major": 1, "minor": 0}

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

- A number means the host will re-ask after at most that many milliseconds.
- `null` means event-driven: the host asks when a terminal changes rather than on
  a timer.

That matters for expensive plugins. `git status` on a large repository should
declare a few seconds, or `null` and let terminal events drive it; either way the
host will not poll it faster than asked.

The refresh clock restarts when an answer arrives, not when the question is sent.
Without that distinction one slow plugin gets asked on every tick — for `git
state` that means forking a subprocess tens of times a second, which starves the
other plugins of response time.

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
- The **minor** version may be lower, so the host can add fields additively
  without breaking existing plugins.

## Failure handling

Plugins are untrusted input. Everything below is discarded and the application
continues:

- A line that is not valid JSON, or does not match the schema.
- A message of a type the host does not know.
- A value out of range: `priority` outside −1000…1000, `text` longer than 512
  characters, or `refresh_ms` above an hour.
- More than 64 fragments in one response.
- A fragment with neither text nor an icon.

A plugin that crashes, exits, or does not answer within the timeout is marked
faulted and stopped. Its content is withdrawn rather than left frozen, and it is
restarted later. The application and every terminal stay usable throughout.

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

Add `{"name": "my-status", "enabled": false}` to the `plugins` array in
`${XDG_CONFIG_HOME:-$HOME/.config}/among/plugins.json`, preserving other entries
and fields. If the file does not exist, create it with:

```json
{"plugins": [{"name": "my-status", "enabled": false}]}
```

The configuration name is the installed filename, not a path or display title.
The host reads registered names at startup and lists only those whose files
exist in the plugin directory. Merely copying a file there does **not** add it
to Settings. Restart Among Terminal when it is safe to close its terminal
sessions, then open Settings → Plugins and enable `my-status`; `Hello` will
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
