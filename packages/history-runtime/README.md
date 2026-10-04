# Native peer interoperability

This private package contains the TypeScript native peer bridge, framed decoder,
invitation parser and progress validation used by the Rust coordination tests.
`scripts/coordination-peer.cjs` drives the bridge to verify interoperability with
`editchain-peer`. It has no VS Code API dependency.

```sh
npm ci
npm test
```

Current application sharing runs in `idle-coordination`. The extension supplies
credentials and commands through its private service pipe; it does not bundle
this package. Join lifetimes and connection policy live in `idle-history` and
are consumed directly by Rust callers.
