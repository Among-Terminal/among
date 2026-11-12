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

Confinement is implemented in [src/plugin/sandbox/](../src/plugin/sandbox/).
Keep enforcement in the pre-exec hook in [launch.rs](../src/plugin/sandbox/launch.rs).
Admission requires a complete report from the spawned PID proving the required
protection. [level.rs](../src/plugin/sandbox/level.rs) must refuse startup without
seccomp regardless of consent; saved consent is a minimum protection floor.
Preserve descriptor-based ownership and mode checks in
[data_dir.rs](../src/plugin/sandbox/data_dir.rs) and the explicit child environment
in [host/environment.rs](../src/plugin/host/environment.rs).

The [user confinement guide](https://among-terminal.github.io/plugins/#confinement)
owns the protection levels and limitations. The template's
[author guide](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/guide.md#settings-and-persistent-data)
owns filesystem access and environment requirements.

## Status rendering

Keep rendering policy in [slots.rs](../src/slots.rs) and shared styling in
[application CSS](../data/among.css). Restrict fragment styles to the host's
`Style` enum and preserve text bounds and icon validation in the renderer.

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
This is the tested copy of the hook on the site's
[sessions page](https://among-terminal.github.io/sessions/#directory-reporting);
both copies must change together. The hook returns the status of the command
that ran before it, so later `PROMPT_COMMAND` entries still see that status.

```sh
among_osc7() {
    local status=$? LC_ALL=C directory uri='file://' byte encoded i
    directory=$(pwd -P && printf '.') || return "$status"
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
    return "$status"
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

The metadata client in [source.rs](../src/plugin/install/source.rs) uses a
5-second connect timeout, a 10-second read timeout and a 60-second operation
deadline. Preserve cancellation and deadline checks across requests, downloads
and extraction.

The [registry](../src/plugin/install/registry.rs) stores custom registrations and
catalogue snapshots in `among/plugin-sources/sources.json` beneath the resolved
data directory, with the official cache in adjacent `official.json`.
Each document is bounded to 8 MiB. Both retained documents plus a proposed atomic
replacement must fit within 16 MiB, with at most four snapshots per source.
Evict old snapshots from the proposed document to make replacement room without
discarding registrations; refuse an operation that still cannot fit.

## Installation transactions

The [store](../src/plugin/install/store.rs) measures staging and generation trees
together, with a traversal limit of 1,048,576 entries. Existing bytes plus the
incoming archive, expanded payload and 256 KiB receipt reservation must fit within
2 GiB overall and 512 MiB in staging. Keep the reservation in admission checks
before downloading a package.

`installed.json` selects installed generations. The
[inventory loader](../src/plugin/inventory.rs) validates selected and previous
receipts before inspecting packages; discovering a directory cannot establish
installation. An unreadable selected payload remains an unavailable inventory
entry rather than silently losing its installation record.

Publication in [store.rs](../src/plugin/install/store.rs) flushes the payload,
`.among-receipt.json` and `.among-lease`, then publishes the fresh generation with
a no-replace rename. Only the subsequent atomic replacement of `installed.json`
selects it. Recheck the store root, previous selection bytes and published inode
before committing. A final directory-sync failure reports uncertain durability.

The generation receipt seal must match the selection receipt. Inventory and the
[process supervisor](../src/plugin/process.rs) retain shared advisory leases;
recovery needs an exclusive lease and skips selected or previous generations.
Remove only sealed, owned, unreferenced generations. Keep the lease alive through
process shutdown, not just while a package is inspected.

New installation and replacement receipts receive fresh `installation` tokens.
Updates preserve the token and require the expected selected receipt. The
[host](../src/plugin/host.rs) binds saved enablement to the current token, so a
replacement cannot inherit an older installation's enabled state. Preserve the
explicit compatibility path for historical receipts without tokens.

## Release revalidation

The [registry](../src/plugin/install/registry.rs) caches the canonical full
release snapshot and tag chain with the catalogue and validates cached
observations before use. [Tag resolution](../src/plugin/install/source/release.rs)
uses the numeric repository and exact `refs/tags/<tag>`, peels at most eight
annotation objects, and fetches the resulting commit by SHA.

The [install worker](../src/plugin/install/source.rs) reobserves the selected
release and complete tag chain with no-cache requests before package transfer
and again after extraction, also rechecking numeric repository and publisher
identity. Reject changes to asset IDs, names, sizes or digests, including
unselected assets. The canonical snapshot ignores asset array order. These
observations establish content consistency; they provide neither independent
publisher authentication nor an atomic transaction with GitHub.

Source receipts retain `release_snapshot` and `release_binding`. Offline
[receipt validation](../src/plugin/inventory.rs) checks release ID, tag,
catalogue digest, selected asset digest and resolved revision against the receipt.
Refuse inconsistent observations without repairing the record. The cached
resolved release holds the complete peel chain; the committed binding holds the
tag, reference object and resolved commit.

### Versions carried over from an earlier release

A release need not ship a plugin that did not change. Its catalogue may list
such a version with `"release": {"id": <release id>, "tag": "<tag>"}`, naming
the earlier release of the same repository that holds the package. The host
reads that release from the release API and applies to it the checks it applies
to the catalogue's own release: published, immutable, the tag resolving to the
version's `revision`, the package among its assets with the stated size and
SHA-256, and the same re-observation before the installation commits. Versions
in one catalogue may therefore describe different release commits.

The reference is refused when it names the catalogue's own release, a draft, a
mutable release, another tag than the release has, or, in a stable release's
catalogue, a prerelease; when the package is also an asset of the new release;
and when the catalogue ships no version of its own. A catalogue may refer to at
most eight earlier releases. A local catalogue whose version carries a `release`
reference is refused: a file has no repository in which to find that release.

The receipt records the referenced release, including the SHA-256 of that
release's own `among-plugins.json` asset as stated by the release API, so the
plugin is the same installation whichever of the two catalogues offered it.
Accepted limit: the host does not download that earlier catalogue or check
whether it lists the carried entry, and the receipt does not retain the digest
of the newer catalogue that offered the version.

A snapshot of a catalogue that refers to earlier releases also holds those
releases as they were observed, under `carried`. This key is written only when
the catalogue refers to earlier releases. A build without support for this key
rejects the whole source registry document that contains it: with the key in
`sources.json` custom sources are not shown, and with it in `official.json` the
official source reports a problem. Reading these files does not rewrite them,
and there is no automatic conversion for older builds.

## Polling and process retirement

The [host poller](../src/plugin/host.rs) rotates its starting plugin each time.
Preserve the [transport bounds](../src/plugin/transport.rs): at most 16 frames per
plugin and 256 globally. Before each frame, stop draining if elapsed polling time
has reached 5 ms. This check does not bound the total duration of `poll()`.

In [process.rs](../src/plugin/process.rs), keep the leader unreaped while sending
group termination signals and checking group quiescence through `/proc`. After
reaping, issue only signal-zero existence queries: `kill(-pgid, 0)` returning
`ESRCH` is the absence proof. Every other result retains the stopping identity,
admission slot and package lease. A descendant that leaves the owned process
group falls outside this supervision.

## Implementation references

The [protocol implementation map](plugin-protocol.md) lists host modules for
transport, admission, configuration, installation and rendering.

See the [managed notification backend](plugin-notification-backend.md) for host
integration and the [sandbox checks](testing.md#plugin-sandbox-hostile-tests) for testing.
