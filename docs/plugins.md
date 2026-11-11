# Plugin host development

User instructions live in the [plugin guide](https://among-terminal.github.io/plugins/).
Plugin authors should use the template's
[author guide](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/guide.md),
[API reference](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/api.md)
and [packaging guide](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/releasing.md).

## Installation authority

Only committed managed installation records enter the plugin inventory. Source
catalogues, repository previews and loose executables do not authorize execution.
The official source pins repository `1431055733` and publisher `339567100`;
matching display names do not establish official identity. Keep these checks in
[official source validation](../src/plugin/install/official.rs) separate from
runtime access grants.

The optional `AMONG_GITHUB_TOKEN` is sent only to `https://api.github.com`, never
to release download hosts. Empty or whitespace-only values are ignored. The
token is not logged or included in error messages. Among removes it from the
environment before starting children, but its original startup value can remain
visible to the same user through Among's `/proc` entry. Preserve that boundary
in [request authorization](../src/plugin/install/source.rs).

## Confinement

Among applies Linux Landlock and seccomp before the plugin executable starts.
The child must report that it applied the required protection. A missing or
insufficient result refuses startup.

With full protection, the plugin can read its installed package and the system
paths `/usr`, `/lib`, `/lib64`, `/etc/ld.so.cache` and `/etc/localtime` where present.
Execution is allowed in the package and system directories. It can read and
write its own data directory, supplied through `AMONG_PLUGIN_DATA_DIR`, but cannot
execute files from that directory. The `system.metrics` grant adds read-only
access to `/proc/stat`, `/proc/meminfo` and `/proc/loadavg`. Its installed package
is read-only.

Repository plugins keep data in
`$XDG_DATA_HOME/among/plugin-data/<repo-id>/<plugin-id>/`, where `<repo-id>` is
the numeric repository ID. If `XDG_DATA_HOME` is unset, empty or relative,
Among uses `$HOME/.local/share` instead. The plugin directory is created with
mode `0700`; startup is refused if an existing leaf directory is not exactly
`0700`.

Seccomp blocks network and local sockets, access to other processes through
debugging and memory APIs, signal sending, shared IPC facilities and selected
kernel operations. It also denies file-metadata changes, including permission
and ownership changes (`chmod` and `chown` families), timestamps (`utimensat`)
and extended-attribute changes. Children inherit the sandbox. The environment
is cleared and rebuilt with `PATH`, `LANG`, optional `TZ`, and the private data
directory; `HOME` and the parent's `XDG_*` variables are not inherited.
If `TZ` exceeds 1024 bytes or contains an ASCII control byte, the plugin is
marked unavailable before its process is spawned.

| Protection available | Startup and file access |
| --- | --- |
| Landlock ABI 3 or newer (kernel 6.2 or newer) plus seccomp | Full protection; no reduced-protection consent needed |
| Landlock ABI 1 or 2 plus seccomp | Explicit consent required; this kernel cannot prevent truncation of writable files outside the allowed paths |
| Seccomp without usable Landlock | Explicit consent required; file access is unrestricted within the user's normal permissions |
| No usable seccomp | Plugin cannot start, even with consent |

If a later kernel provides less protection than the user accepted, Among asks
again. The sandbox does not impose CPU or memory quotas and does not protect
against kernel vulnerabilities. Its process supervisor is a separate mechanism:
a descendant that deliberately leaves the owned process group remains sandboxed
but is no longer stopped with that group.

## Status rendering

[Slot rendering](../src/slots.rs) controls colours, font and layout, shortens
long text, and drops unavailable theme icons. Meters have a minimum width of
50 px and change colour continuously from green through yellow to red as the
value rises from 0 to 1. [Application CSS](../data/among.css) gives shared header
and footer labels bold monospace text, footer labels opacity `0.7`, and the
`dim` style opacity `0.55`.

For ordinary slots, polling timing starts from the response; backpressure can
delay the next request. The host applies responses and schedules collection in
[the application](../src/app.rs). The API's refresh field and sidebar exception
are documented in the
[status reference](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/api.md#status-and-fragments).

## Live status examples

The live status checker reads and executes this OSC 7 hook from this file.
The regression test also executes it with spaces, reserved URI bytes, Unicode,
a newline and a symbolic link in the working directory, with external commands
unavailable. Keep the hook valid for both consumers:
[live status setup](../tools/among-tools/src/live_status.rs) and
[OSC 7 regression test](../tools/among-tools/tests/live.rs).
The Snap workflow copies this file into its checker input.

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

User setup instructions live under
[directory reporting](https://among-terminal.github.io/sessions/#directory-reporting).

## Source management

Catalogue age, publisher review and source-check failures are documented under
[plugin sources](https://among-terminal.github.io/plugins/#plugin-sources).
Implementation entry points are the [source registry](../src/plugin/install/registry.rs),
[plugin manager](../src/prefs/plugin_manager.rs) and
[source controller](../src/prefs/sources.rs).

## Implementation references

The [protocol implementation map](plugin-protocol.md) lists host modules for
transport, admission, configuration, installation and rendering.

See the [managed notification backend](plugin-notification-backend.md) for host
integration and the [sandbox checks](testing.md#plugin-sandbox-hostile-tests) for testing.
