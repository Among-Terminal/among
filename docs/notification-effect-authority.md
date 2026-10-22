# Owned notification effect authority

This GTK-free library boundary transports core revocation through exec. It does
not wire audio playback or General controls. The unshipped
`examples/effect-authority-helper.rs` exercises a one-byte synthetic effect, not
a decoder or an audible-output acceptance test.

## Issue once from the exact delivery

`Backend::authorize_delivery(job, now)` privately issues a `DeliveryAuthorization`
only with valid event metadata, kind/source/origin attribution and original
ten-second lifetime. Obtain `Host::authorization(delivery.job())`, then call
`delivery.effect_authorization(&authorization, now)`. A different job is refused.
Managed work requires its exact retained origin lease/identity; origin absence is
valid only for an explicitly issued host bell/application snapshot.

**Missing metadata means missing at original trusted issuance.** An already
privately issued immutable snapshot survives eviction from the bounded backend
event cache. Conversion does not require a Backend borrow or add cache-based
cancellation. Source, channel/configuration, registration, session and origin
revocation, plus original expiry, still apply.

`EffectAuthorization: Clone + Send + Sync` owns its exact `job()` and
`expires_at()`. `is_current(now)` reads retained scopes and expiry without GTK,
Host/RefCell borrows, callbacks or I/O. This is authority to attempt an effect,
not a receipt or a unique operation: the trusted dispatcher must bind clones to
one owned operation, preventing automatic replay or multiple fresh operation
nonces for the same dispatch. Clone/export never renews the event lifetime.

## Same state, direct writers

Each lifetime eagerly creates a shared memfd scope. Core mutators write its
original mapped atomic; helpers read that same atomic, not an export-time copy:

| Terminal scope | Direct revocation |
| --- | --- |
| Session | Host Drop/replacement |
| Source generation, if present | Detach/replacement attach |
| Channel revision | Policy change, `cancel_channel_work` even if policy is unchanged, removal |
| Registration | `Registration::revoke`, guard Drop, channel removal |
| Managed origin, if present | Direct `OriginLease::revoke`, core retirement, backend revoke/replacement/uninstall/Drop |

`Host::registration(channel)` returns an owned, non-cloneable `Registration`.
Retain it in the adapter's registration map; dropping any such guard terminates
that registration. Read-authority clones cannot revoke. Revocation is a SeqCst
store of zero: no allocation, FD reopen, subscriber list, helper lock, propagation
callback, GUI tick, supervisor copier or additional queue. New lifetimes use new
cells/nonces; no path stores live again. Revision overflow or replacement-cell
allocation failure leaves the old revision revoked.

Cell creation performs bounded memfd/mmap/seal/random operations at lifetime
creation, including Host/source/channel registration. This is an explicit new
resource boundary, not hidden lazy work inside `is_current` or a helper handshake.
`Failure::AuthorityUnavailable` refuses that admission. Source replacement retires
the old generation before allocating its successor, so allocation failure leaves
no old source authority live. Export/import/dup failures
are explicit `EffectAuthorityError`s and close partial resources; callers must
not omit a failed scope, default true, or queue a retry.

## Export and owned inheritance

`export_process()` returns a `ProcessEffectAuthorization: Send + Sync` owning a
read-only sealed manifest and three to five read-only scope FDs. The bounds are
**six FDs total, 512 binding bytes**, not five total FDs. `binding()` is the expected
immutable descriptor; `duplicate_fds()` returns a non-cloneable
`EffectDescriptors`. Retain that wrapper through spawn and borrow `as_slice()`
for deliberate inheritance. Its Drop closes parent copies and releases their
budget together. All FDs default to CLOEXEC; clear only in the owned helper's
fork/exec path. `import_for_helper(expected_binding, Vec<OwnedFd>)` restores
CLOEXEC before native scanner children can be started.

The **trusted owned-operation route** must supply `expected_binding`; accepting an
arbitrary self-consistent binding from received FDs does not authenticate an
issuer. Import checks canonical role/count/order, exact job/deadline, stat
device/inode, scope/capability nonces, sizes, access modes, seals, reserved bytes
and time namespace. Missing/extra/aliased/reordered/writer/unrelated or stale
FD-number substitutions refuse before native construction. No PID/path string
is an identity token. `/proc/self/fd` is used only to reopen already owned FDs
O_RDONLY, not to discover another process's authority.

A global per-process reservation limits library-managed live handles to
`MAX_EFFECT_HANDLES = 4096`, including retained generations, process imports,
descriptor wrappers and conservative temporary export reservations. Scope clones
share one handle/reservation. This is not a limit on arbitrary caller-made FD
duplicates; consumers must retain the bounded wrappers rather than build a cache
of unaccounted duplicates. Memfd backing per scope is two checked system pages
(4–64 KiB per page), plus one 512-byte manifest per process export.

## Final start ordering

Finish import/mapping, validation, native construction, and blocking protocol/I/O
**before** the final sequence:

1. Successfully claim the separate owned audio/command operation with its SeqCst
   CAS and nonce checks.
2. Immediately call `process.validate_after_claim()`.
3. On success invoke the already-prepared effect directly; on failure never play,
   regrant or retry that operation.

The method performs a SeqCst fence, ordered SeqCst atomic scope reads, then a
**fresh local CLOCK_MONOTONIC** read. On success every scope was live after claim;
because each only changes live→revoked, each was also live at claim. The later
unexpired time proves claim preceded expiry. This preserves the successful
operation CAS as the start point, unlike independent pre-CAS live reads.
`is_current_monotonic(now_ns)` is an admission/active check, not this proof: its
caller-supplied time can be stale.

This protocol relies on coherent shared mappings and lock-free aligned 32-bit
atomics on supported Linux targets (including x86-64/AArch64), with SeqCst
revocation/claim/final reads; it does not serialize Rust struct layout. There is
no allocation, mapping, callback or protocol I/O in the successful final method;
clock_gettime may use the vDSO or a syscall. No constructor or handshake belongs
between that sequence and the effect. Revocation after start requests retirement;
it does not undo output, eliminate OS descheduling, or guarantee immediate silence.

Expiry is original `received_at + 10s`, using checked arithmetic. Issuance samples
CLOCK_MONOTONIC **before** `Instant::now()` and adds the remaining original lifetime
to the earlier sample, so a pause shortens rather than extends permission. Export
retains that absolute deadline. Issuance/export/import require the same Linux
time-namespace device/inode; the helper must not change namespace afterward.
At `now >= deadline` authority refuses. The consumer separately enforces its
operation deadline (audio's existing 13-second budget is not reset or replaced).

Ingress focus eligibility remains snapshotted by core; this adds no helper-side
focus polling. Later focus cancellation needs a defined direct revocation route.
Normal Drop revokes, but SIGKILL/abort does not run Drop: retained live mappings
can remain live until expiry. Existing consumer EOF/parent-death/deadline handling
is still required. Seals/read attenuation are cooperative ownership, not a native
code sandbox or cryptographic same-user authentication.

## Version 1 Linux layout

All encoded integers are fixed-width little endian; reserved bytes are zero.
Scope roles are session=1, channel=2, registration=3, source=4, origin=5 in that
order, with the latter two present only under the exact authenticated mask.

Each scope memfd is exactly two system pages. The first 64 bytes are:
magic `AMNGRV01` (0..8), u16 version (8..10), u8 role (10), reserved (11..16),
16-byte nonce (16..32), u64 session/key/generation (32/40/48), reserved (56..64).
The first owner page is mprotect read-only; the aligned native AtomicU32 at the
second page boundary is initially 1, then terminal 0. Only the original core
mapping retains write authority. GROW/SHRINK/FUTURE_WRITE/SEAL seals prevent
resize, pwrite and new writable shared mappings, including via a reopened FD.
The trusted owner must not undo page protections; this is not protection against
arbitrary native code in the owner process.

The manifest memfd is exactly 512 bytes, with WRITE/GROW/SHRINK/SEAL seals. Its
binding has a 128-byte header followed by 40 bytes per scope (248–328 bytes total),
then zero padding to 512:

| Offset | Header field |
| --- | --- |
| 0 | 8-byte `AMNGEF01` |
| 8 / 10 / 11 / 12 | u16 version / u8 scope count / u8 source+origin mask / u8 event kind (bell=1, application=2, managed=3) |
| 13..16 | Reserved |
| 16 / 24 / 32 / 40 | u64 session / event / channel / revision |
| 48 / 56 / 64 / 72 | u64 pane / source generation / origin token / absolute monotonic deadline ns |
| 80..96 | 16-byte capability nonce |
| 96 / 104 / 112 / 120 | u64 manifest device / inode / time-namespace device / inode |

Each scope record stores role at 0, reserved 1..8, u64 device/inode at 8/16,
and nonce at 24..40. Identity headers must match the exact job-derived roles,
session, channel revision, source generation and origin token.

## Local boundary probes

Build examples before running `cargo test --locked --test effect_authority`.
The tests retain helper logs/bindings below `AMONG_CHECK_ROOT` (or the test-local
target evidence directory). They exercise real exec, direct independent writers,
off/on and lifetime reuse, original expiry, malformed bindings/FD sets,
read attenuation, scanner-child CLOEXEC, claim ordering, cache eviction and
isolated FD/budget exhaustion. Library tests also exercise forged private
snapshots and conservative clock conversion. Synthetic output evidence does not
establish actual player integration, captured silence, native General ingress,
packaging or platform acceptance.
