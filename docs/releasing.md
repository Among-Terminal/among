# First publication guide

Release builds and publication are separate operations. A successful build does
not authorize publication: package acceptance, source delivery, credentials and
owner approval must be established for the exact artifacts being published.

### Two-architecture promotion records

Public promotion requires schema 2. The record's `architectures` map must contain
both `amd64` and `arm64`, each mapping `snap`, `source` and `association` to
distinct filenames in the digest-bound `assets` map. Gate receipts include all
six checks for each architecture, in addition to the aggregate checks. Packaged
architecture is read from each Snap, not inferred from its filename. Each source
bundle and association is checked against its Snap; stable source inputs must
match the tag for both bundles.

`among-tools check-promotion-admission RECORD GATES ASSETS --approved` enforces
the read-only half of this contract today: approval, provenance, exact gate and
digest binding, all six gates per architecture and the complete artifact set. It
returns `published: false` and performs no endpoint operation. Schema-1 records
remain readable for historical offline fixtures but cannot authorize the first
public release's two-architecture payload.

The publishing half is `among-tools release-promotion` (below). Its GitHub
adapter drives `/usr/bin/gh`; its Store adapter uploads and releases with
`/usr/bin/snapcraft` and reads each revision's `sha3-384`, version and
architectures from the Store HTTPS API with `/usr/bin/curl`, because no
Snapcraft CLI command prints that digest machine-readably. Those adapters are
tested only against stand-in programs; they have not run against the real
GitHub or Store services. Do not substitute human-readable table parsing for
the digest check, and do not treat a successful `check-promotion-admission` as
evidence that anything was published.

Sources for both architectures become public before any Store channel advances.
A partial Store failure leaves the immutable release available and does not
advance the nightly success marker. Retry reconciles completed revisions by
architecture and exact digest. Schema 1 remains readable in historical offline
fixtures but is refused by the public promotion CLI; the old single-architecture
rehearsal is not evidence for this new release model.

## 1. Establish ownership and protections

The source repository is `Among-Terminal/among`; distribute the original code
with the applicable third-party attribution retained. Enable immutable releases
before the first publication (repository settings, "Releases", or
`gh api --method PUT repos/Among-Terminal/among/immutable-releases`); it applies
only to releases published afterwards.

Protect `main`: require pull requests, review and the Checks job; disallow force
push/deletion. Restrict creation/update/deletion of `v*` tags to authorized release
maintainers. Create a protected `publishing` environment with required reviewers,
prevent self-review where available, and restrict deployments to release tags or
the intended main nightly workflow. Confirm your GitHub plan supports the needed
environment/ruleset enforcement; a YAML name alone does not protect secrets.

Create a Snapcraft developer account through Ubuntu One. In your own terminal:

```sh
snapcraft login
snapcraft register among
```

`among` is a candidate name, not a claim of availability. If unavailable, resolve
the name/command design before changing metadata. Follow the official
[classic review guide](snap.md): host shell/tool access requires classic
confinement and Store approval before distribution. Fulfil publisher verification
requested during review; no approval has been obtained here.

## 2. Prepare the listing and artifact

Use the original `data/com.github.among-terminal.svg`, accurate screenshots and the
approved silent README promo or [feature demonstrations](features.md). Describe classic host access,
supported architecture and verified behavior honestly. Do not show private paths,
account names, shell history or credentials. Fill contact/source/issue URLs only
after the repository is available.

Run both package checks, license inventory checks and live native-X11 scenarios
from [CONTRIBUTING](../CONTRIBUTING.md). Build the Snap in its Ubuntu core24
environment, not by copying the Debian executable. Inspect the actual staged
libraries, source materials, licenses, loader/RPATH and [LGPL obligations](licensing.md).
Test the exact package on a dedicated machine before considering publication.

The source recipe has `grade: devel`, which prevents stable promotion of its
candidate packages. Installed core24/amd64 candidate checks have passed in a
disposable KVM/X11 guest, including the context menu. That evidence does not
automatically cover a new stable-grade build: retest its exact bytes before
promotion, as well as source/license/privacy gates.

The owner approved a separate stable build tree, rather than changing that source
recipe in place. From a reviewed source checkout whose two manifests match the tag:

```sh
tools/among-tools/target/release/among-tools prepare-stable-build . ../among-stable-source v0.1.0
tools/among-tools/target/release/among-tools check-stable-build ../among-stable-source
```

The destination must be new, with an existing parent outside the checkout. Only
explicit public source inputs are copied, not `.git`, build caches, private config
or local recording workspaces. Build Snapcraft from this resulting directory, not
from the original checkout. The effective recipe changes only `grade` to stable;
the original is retained as `snap/snapcraft.devel.yaml`, alongside the input-hash
record `snap/stable-build-input.json`. Both are included by the source-snapshot
tool, so the distributed sources describe the actual build overlay.

This helper does not authenticate a Git tag or authorize a release. The protected
workflow must independently verify the immutable tag/main commit and run all gates
on the resulting artifact. Offline overlay checks:
`cargo test --locked --manifest-path tools/among-tools/Cargo.toml --test stable_build`.
The Rust overlay preserves the source recipe and selected input bytes/modes,
rechecks them before output creation and refuses existing or in-checkout output.
Publication approval and full corresponding-source validation remain separate.
Stable promotion now also checks each archived original public source file against
the actual tag's Git blob, allowing only the recorded grade overlay. A self-consistent
source/binary digest association for another commit is insufficient.

## 3. Keep credentials scoped and expiring

Never paste credentials in chat, commit them, attach them to build artifacts or
enable shell tracing around them. PR jobs get read-only permissions and no Store
secret. Protected promotion must use the narrowest destination-specific access.

After approval, export separate edge/stable credentials on your own machine. Use
an explicit future ISO-8601 expiry suitable for your release window:

```sh
umask 077
snapcraft export-login --snaps=among --channels=edge \
  --acls=package_access,package_push,package_release --expires=<ISO-8601-expiry> edge.login
snapcraft export-login --snaps=among --channels=stable \
  --acls=package_access,package_push,package_release --expires=<ISO-8601-expiry> stable.login
```

Replace placeholders locally. Check current CLI help/Store ACL support before
exporting; never use stdout as the destination in a recorded terminal.
`SNAPCRAFT_STORE_CREDENTIALS` must hold the exported file's contents, not its
path, as Snapcraft's
[Authenticate](https://ubuntu.com/docs/snapcraft/8/how-to/publishing/authenticate/)
guide requires; `release-promotion` refuses a value that is not an exported
Ubuntu One login before contacting any destination. Put values
only in the protected environment's secret manager when the publication workflows
are implemented, then remove the local files securely according to your system's
storage policy. Rotate before expiry or immediately on suspected compromise.
Do not configure a real publishing secret merely to run offline tests.

## 4. Stable version and immutable provenance

Choose a strict increasing `vMAJOR.MINOR.PATCH`, with matching versions in both
Cargo manifests and AppStream metadata. Run all gates on its immutable commit.
The commit must be reachable from protected main; use a protected tag, never a
mutable branch head as release provenance. A local policy example is:

```sh
tools/among-tools/target/release/among-tools release-policy . v0.1.0 refs/remotes/origin/main ./among_0.1.0_amd64.snap
```

For later releases pass `--previous` with the last published provenance JSON. The
policy resolves the tag commit, reads both manifests from that commit, rejects
old/malformed/untrusted tags, hashes assets and accepts an exact repeat only if
all immutable provenance matches. It does not build, approve or upload anything.
Offline fixtures:
`cargo test --locked --manifest-path tools/among-tools/Cargo.toml --test release_policy --test release_metadata`.
Use `--architectures roles.json` to produce schema-2 provenance with distinct
checked Snap/source/association assets for both amd64 and arm64. The schema-1
example is retained only for initial amd64 compatibility, not six-target stable
release acceptance. `among-tools check-release-metadata .` checks the candidate
recipe, both versions/licenses, AppStream/desktop identity and override shell
syntax; it does not build or test a Snap. Execute project tools in containers.

Promotion is run with the Rust entry point. It re-derives stable provenance
from the protected checkout, verifies the packaged Snap identity per
architecture, and only then contacts GitHub and the Store. It publishes: it
creates and publishes the GitHub release, uploads and releases each
architecture's Snap, and for a nightly rolls the success marker. It therefore
needs `GH_TOKEN` and `SNAPCRAFT_STORE_CREDENTIALS` and belongs only in a
protected release job:

```sh
AMONG_PROTECTED_APPROVAL=yes \
among-tools release-promotion \
  /path/to/record.json /path/to/gates.json /path/to/artifacts \
  owner/name /path/to/checkout
```

Without `AMONG_PROTECTED_APPROVAL=yes` the command refuses before it reads a
record, a gate or an artifact. No workflow invokes it yet.

The portable policy, staging and identity checks remain:

```sh
cargo test --locked --manifest-path tools/among-tools/Cargo.toml \
  --test promotion --test promotion_nightly --test promotion_preflight \
  --test promotion_destinations --test adapter_processes --test nightly
tools/among-tools/target/release/among-tools check-release-metadata .
```

`check-promotion-admission` checks explicit approval, strict stable provenance,
exact source/artifact gate binding, all six gates and both architecture
receipts, and real artifact bytes. It is read-only and returns
`published: false`; it does not perform any endpoint reconciliation or upload.
It accepts a nightly record as well as a stable one; nightly publication itself
remains deferred because no workflow schedules it.

The Rust suites above test approval/gate refusal, immutable asset/source
conflicts, numeric stable version order, zero-write unchanged runs, partial
failure recovery against an in-process stand-in for both destinations, and the
adapters' read paths (release and marker lookup, Store revision metadata),
credentials and error handling against stand-in `gh` and `curl` programs. The
adapters' write paths (GitHub prepare, publish and roll; Store upload and
release) are not driven by any test yet. None of this is proof of
authenticated hosted execution.
No daily workflow exists. The only tag workflow that writes to GitHub creates a
release draft (section 5); it publishes nothing, and no automated publisher
should be considered operational yet.

The migrated Rust selector is retained as deferred functionality. Its offline
tests use disposable Git objects and mocked read-only `gh api` responses. It
resolves main once, skips only a complete matching marker, retains the original
version on prepared retries, and refuses authentication failures instead of
treating them as missing releases. Output creation is exclusive; endpoint error
text and credentials are not returned. Manual read-only use in a container:

```sh
among-tools select-nightly . OWNER/REPO ./new-selection.json
```

This command only selects; it does not build, upload, publish, replace rolling
assets or advance a Store channel. No schedule or edge promotion is activated.

The adapter publishes the immutable GitHub release **including sources before**
advancing the Store channel, so newly served binaries already have corresponding
source downloads. A Store failure leaves that GitHub release intact but does not
advance nightly success. The rolling preview is marked `rolling` before its
assets change and `complete` only after both destinations and all preview writes
succeed. Each `nightly-<full-SHA>` release preserves historical source materials.
The nightly selector can resume a prepared release using its original version;
a next-day retry must download/reuse those checked artifacts, not rebuild them
with a new date. Missing assets in an already public immutable release are a
hard failure, not permission to silently add/replace bytes.

Publication also checks the remote tag's peeled commit, packaged identity and
grade, and the actual binary/source association before contacting the Store.
Store revisions require the exact SHA3-384, not matching names or version alone.
Missing digest fields fail closed. Credential-dependent errors are not echoed.
`AMONG_PROTECTED_APPROVAL=yes` is only a local intent guard: it is **not** a
substitute for the GitHub protected environment/rulesets described above.

The planned schedule is 03:17 UTC daily on main, with a rolling GitHub `nightly`
prerelease and Snap edge. Resolve main once; compare the immutable commit to the
last fully successful publication marker. Unchanged commits must make zero
publication calls. Include UTC date/source SHA in the bounded Snap version.

Serialize promotion, reconcile matching existing artifacts on retries and advance
the success marker only after both destinations succeed. A partial failure is not
success and must remain retryable. Caches are not success markers. Document a
manual rerun against the original commit, not today's new branch head.

Actions must be explicitly enabled after review. No scheduled or publishing
workflow is currently present, so enabling Checks alone does not publish
nightlies; a release tag only fills a draft (section 5). Once scheduled
workflows exist, disable the specific scheduled workflow in GitHub Actions when
pausing nightlies; preserve stable controls and prior provenance.

## 5. Release drafts from a tag

Pushing a `v*` tag starts `.github/workflows/release-draft.yml`. In one run it
calls `deb-release.yml`, `rpm-release.yml` and `appimage-release.yml`, which
build and install-check every package for amd64 and arm64. Only if all of their
jobs pass does it fill the tag's GitHub Release draft; any failure leaves
releases untouched. The DEB and RPM workflows have no trigger of their own; they
run only when called this way.

| Job | Token | Does |
|---|---|---|
| `deb`, `rpm`, `appimage` | `contents: read` | build and installed checks |
| `assemble` | `contents: read` | `among-tools stage-release-assets`: selects, binds and checksums the files; `among-tools release-notes`: renders the description |
| `draft` | `contents: write` | creates or reconciles the draft, uploads, verifies; runs nothing it downloaded |

`assemble` reads exactly these workflow artifacts and nothing else:
`deb-{debian13,ubuntu24.04}-{amd64,arm64}` (the `.deb` and the binary archive),
`rpm-{fedora43,fedora44}-{amd64,arm64}` (the binary RPM),
`rpm-checked-{fedora43,fedora44}-amd64` (the SRPM),
`appimage-{amd64,arm64}` (`among-<version>-<arch>.AppImage` and its audit
record) and `appimage-sources-{amd64,arm64}` (the Debian source packages of the
libraries each AppImage bundles, with `sources.json`). Every DEB, archive and
RPM must match the tag, the commit and the digest recorded by its build; the
SRPM must come from the build of the attached amd64 RPM (both architectures
produce an SRPM with the same name and different bytes); the RPM workflow's
older upgrade-test package in `ci-lifecycle-fixture-not-for-release/` is never
selected. Each AppImage must be an ELF executable for its architecture and match
its audit record. Its sources must name exactly the source packages the audit
lists, every descriptor and file must verify, and nothing else may be present;
they are attached as one tar per architecture, because the LGPL libraries in the
AppImage must be accompanied by their corresponding source. Checker binaries,
build records, audit records and runtime evidence are not attached.

The draft holds nineteen assets under version-free names, the same for every
tag, so `releases/latest/download/<name>` (linked from the README) always
fetches the newest stable file:

| Files | Attached as |
|---|---|
| four DEB | `among-terminal-{debian13,ubuntu24.04}-{amd64,arm64}.deb` |
| four binary archives | `among-terminal-{debian13,ubuntu24.04}-{amd64,arm64}.tar.gz` |
| four RPM | `among-terminal-{fedora43,fedora44}-{amd64,arm64}.rpm` |
| two SRPM | `among-terminal-{fedora43,fedora44}.src.rpm` |
| two AppImages | `among-terminal-{amd64,arm64}.AppImage` |
| two AppImage source archives | `among-terminal-{amd64,arm64}-appimage-sources.tar` |
| checksums | `SHA256SUMS` |

Build outputs keep their versioned names; only the attached name differs.
GitHub limits a release to 1000 assets, each under 2 GiB, and renames asset
names with special characters; the attached names use only letters, digits,
`.`, `_` and `-`. After uploading, the run downloads every asset of the draft
and requires `sha256sum --check SHA256SUMS` to pass. Tags with `-rc.N` produce
a prerelease draft.

The draft's description is rendered by `among-tools release-notes` from the
staged `SHA256SUMS`: the highlights in `.github/release-notes/<tag>.md`, a
download table linking every file of this tag once, install and verification
commands, the scope of the installed checks, source materials, commit and run.
A stable tag without a non-empty highlights file fails in `assemble`, before any
release exists; release candidates may omit it. The description travels as the
separate `release-notes` artifact, and the draft job creates the release with it
as `body` and `generate_release_notes=true`, so GitHub appends the merged pull
requests grouped by `.github/release.yml`. A repeated run keeps an existing
draft's description.

Repeated runs never replace bytes. A published release for the tag, more than
one release for it, an existing draft whose prerelease flag disagrees with the
tag, a draft asset outside the expected set or with other bytes, or a tag that
no longer points at the built commit fails the run without changes. A draft with a matching subset gets only the missing assets, so
"Re-run failed jobs" after an interrupted upload is safe. A rebuild produces new
bytes; to rebuild, delete the draft first. The `rpm-checked-*` artifacts expire
after five days, after which a re-run must rebuild from the start.

The draft is not a publication. Download every asset into an empty directory,
run `sha256sum -c SHA256SUMS`, review the notes, then publish the draft in the
GitHub interface. That click is the owner's approval. The repository has
immutable releases enabled: once published, a release's assets cannot be
changed or deleted, its tag cannot move, and a deleted release's tag name
cannot be reused, so a defect found after publishing ships as a new version.
GitHub records an attestation for each published release, which
`gh release verify <tag>` and `gh release verify-asset <tag> <file>` check.

After a stable release is published, update
`packaging/ci/deb-upgrade/previous-release.json` in a pull request: set `tag`
to the new release and, for each Debian slot, the attached file's SHA-256 from
its `SHA256SUMS` and the package version inside it. The next release's
installed DEB checks then upgrade from the release users actually have; the
`deb_previous` tests refuse a pin whose names or versions do not follow from
its tag.

Offline checks:
`cargo test --locked --manifest-path tools/among-tools/Cargo.toml --test release_assets --test release_notes --test readme_downloads --test release_draft_workflow -- --include-ignored`.
The tests that execute the draft step need `jq`, as the GitHub-hosted runner
provides; they are `#[ignore]`d so that package builds, whose containers have
no `jq`, skip them, and the Checks workflow runs them with its ignored-test
runner. Without `jq` the draft step stops with `jq is required on the runner`
before any GitHub call.

## 6. Publication, verification and rollback

The following are owner-only external operations, **not executed here**:

```sh
snapcraft upload --release=edge ./among_<nightly-version>_amd64.snap
snapcraft upload --release=stable ./among_<stable-version>_amd64.snap
snapcraft status among
snapcraft revisions among
```

Use only the approved, tested artifact. Verify GitHub source SHA/asset hashes and
the Store revision/channel/version after publication. On a separate test machine
install with `snap install --classic among`, verify the listed version and repeat
host-tool/project/plugin/focus checks. Never assume an upload means runtime passed.

Rollback by re-promoting an already verified prior Store revision with
`snapcraft release among <known-good-revision> stable`, then verify channel status.
Do not rewrite immutable stable Git tags or replace historical release assets.
For a local test installation, `snap revert among` is a separate machine-local
operation, not a replacement for channel rollback. Preserve user config/data.

## Current evidence and blockers

Checks YAML passes actionlint; its local native equivalents have run, but hosted
Actions have not. Offline stable policy fixtures use disposable repositories and
fake assets and contact no publishing endpoint. Those results are not a hosted
release rehearsal. Snap build/runtime/source compliance, protected publication
and retry-safe nightlies, reviewed video, and Store/GitHub setup remain separate
gates. No credential is requested in chat.

## Official command sources (checked 2026-10-07)

- [Register a snap](https://ubuntu.com/docs/snapcraft/9.1/how-to/publishing/register-a-snap/index.html.md)
- [Publish a snap](https://ubuntu.com/docs/snapcraft/9.1/how-to/publishing/publish-a-snap/index.html.md)
- [Export scoped credentials](https://ubuntu.com/docs/snapcraft/9.1/reference/commands/export-login/index.html.md)
- [Classic/core24 source checks](snap.md)

These are current official Markdown sources selected through the Snapcraft llms
index. Export/register/upload commands are documented, not actually executed.
