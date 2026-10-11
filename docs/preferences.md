# Preferences storage

The storage module uses `$XDG_CONFIG_HOME/among`, falling back to
`$HOME/.config/among` when XDG_CONFIG_HOME is absent or empty.

Plugin choices keep the existing `plugins.json` format:

```json
{"plugins": [{"name": "clock", "enabled": false}]}
```

Shortcut overrides use `shortcuts.json`, version 1:

```json
{"version": 1, "overrides": {"split-pane": "<Control><Shift>b", "toggle-zoom": null}}
```

An absent override inherits the current default; null explicitly removes a
binding. Unknown fields and entries are retained when saving other preferences.
Malformed files and unsupported versions are not automatically overwritten.

Writes use a private temporary file in the destination directory and atomic
replacement. Failures before replacement preserve the prior document; failures
confirming directory durability after replacement report uncertainty. Symlink
destinations are refused. Independent external edits are merged when detected;
simultaneous writes by external editors are not fully serialized.

## Changing preferences

Accepted edits in both Preferences pages apply immediately and are saved without
a Save button. Fresh launches restore plugin choices and shortcut overrides.
Reassigning an occupied shortcut also saves the displaced action as unbound.
Cancelled capture and rejected/no-op edits do not write new overrides.

A failed write leaves the edit active for this session and shows a warning in
Preferences and a notification. A later edit in the same settings category retries
pending changes; successful saving clears that category's warning. Repair malformed
files or directory permissions before retrying. Startup errors are visible too.
Plugin faults do not change the saved desired enabled state.

Clock, resources and pane PWD remain optional and disabled by default. Only explicit
user changes enable them. No terminal contents or workspace layouts are saved here.

For an isolated native X11 restart check, build the headless Rust tools, application
test executables and example plugins first:

```sh
cargo build --release --locked --bins --manifest-path tools/among-tools/Cargo.toml
cargo build --release --locked --bins --manifest-path examples/plugins/Cargo.toml
mkdir -p /tmp/opencode
cargo test --locked --no-run --message-format=json > /tmp/opencode/preferences-tests.jsonl
tools/among-tools/target/release/among-tools ci-x11 \
  tools/among-tools/target/release/among-tools check-preferences-from-cargo \
  /tmp/opencode/preferences-tests.jsonl examples/plugins/target/release
```

Xvfb, Openbox, xdotool, xprop, D-Bus and ImageMagick `import` must be available.
The checker uses fresh processes, private D-Bus and temporary HOME/XDG directories;
it never terminates personal terminals. This is source regression coverage, not
installed-package acceptance. Session consent/checkpoint failure scenarios use
the companion `ci-x11 ... check-sessions /absolute/path/to/among` command.
