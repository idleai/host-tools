# Dev Tunnels SSH buffer compatibility

This private workspace package satisfies Microsoft's SSH fork's
`russh-cryptovec` 0.7 API using fixed upstream `russh-cryptovec` 0.60.3. Its local
version 0.7.4 identifies this compatibility package, not an upstream release.
The root Cargo patches select it for both registry and fork dependencies.

The original buffer predates the allocation/growth fixes in RUSTSEC-2026-0153.
This wrapper implements no allocation or unsafe code. It delegates storage,
growth, copying and zeroization to upstream, restoring the old `push_u32_be`
method and the inherent `resize` receiver behavior needed by existing SSH code.
Tests cover encoding, growth, erased truncation and overflow rejection before
mutation. The workspace lint policy applies in full.

Keep this package, the pinned SSH fork and Microsoft Dev Tunnels SDK in one
upgrade review. Run `scripts/check-tunnels.py`, native interoperability tests and
the live relay smoke described in [native coordination](../../docs/coordination.md).
Remove the wrapper when the selected SDK's SSH dependency directly supports the
fixed buffer API. Do not patch or ignore advisory results to retain an old buffer.
