# Changelog

User-visible changes of the application, newest first. Written by `among-tools release-prepare` from the titles of the merged pull requests.

## 0.2.0 - 2026-11-05

### Features

- release: tag a release candidate before the stable release (#65)
- plugin: confine managed plugins with Landlock and seccomp (#63)
- notifications: consolidate owned audio authority and live consumers (#53)
- plugins: latest/schema2 installation and plugin development mode (#45)
- move panes between workspaces by dragging
- show Split Pane inactive in an expanded pane's menu
- open terminal links with Ctrl+click
- reorder workspaces with native sidebar dragging
- plugins: forward bounded timezone values to managed children
- plugins: install and manage verified GitHub packages (#33)
- plugins: preserve typed session edits and confirm generation-bound restarts
- plugins: preserve identity-scoped inventory and native search
- plugins: admit managed runtimes with scoped grants and live settings
- plugins: inspect managed metadata and edit validated offline settings
- panes: add target-safe restore control to expanded headers

### Bug fixes

- release: show the version's highlights on a release candidate (#70)
- packaging: make the Debian and RPM package builds pass their test phases (#69)
- appimage: carry the runtime notice in every image (#59)
- appimage: download build tools only from our own release
- harden pane moves between workspaces after review
- say that the hash, not the tag, authorizes appimagetool
- pin appimagetool to the 1.9.1 release
- keep focus on an expanded pane
- keep reordered divider targets and strengthen native acceptance
- retry shared plugin capacity without spending crash restarts
- admit an identity whose detached reservation expires during pruning
- retain visible cross-window stop state until reservation clears
- isolate owned-group retirement across plugin windows
- tooling: pin tar 0.4.46 and regress archive advisories
- deps: update Tokio to the patched 1.54.1 release
- plugins: refresh applied content and reject stale access reviews
- preferences: bound actual managed document output before replacement
- preferences: preserve keyboard toggles and release closed plugin rows
- plugins: retain bounded startup and deterministic pipe regressions
- plugins: bound cancellable retries for busy executables
- plugins: withdraw sidebar summaries through Preferences disable
- plugins: defer status bursts and gate replacement generations
- plugins: bound transport and supervise process generations off-loop
- appimage: supply visible launcher thumbnails and test decoded pixels
- packaging: bind RC metadata and resolve registered file launches
- packaging: align artwork and AppImageLauncher window identity
