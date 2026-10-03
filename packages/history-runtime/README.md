# Portable history runtime

This package owns the existing history peer coordinator, discovery, invitations,
sharing scope, progress and native worker bridge. It was extracted from the
EditChain extension without changing the invitation, worker or saved-state
formats.

Hosts inject the relay transport, credentials, persistence and idle-peer-state
factory. The VS Code adapter lives in
`vscode-extension/extension/src/sharing`; it supplies Dev
Tunnels, SecretStorage and the packaged peer-state WASM module. The package has no
VS Code API dependency.

```sh
npm ci
npm test
```

The VS Code checks run hosted sharing, reconnect, cleanup and native replication
integration tests against this package. The host uses a local npm file dependency
and bundles the runtime in its VSIX. Relay ownership markers accept both the
original host spelling and `idle-relay-` so a host can retain its own resource
journal without translating saved leases.

The f18 standalone Evo coordination service and its configuration/provider
connections remain separate roadmap work. This move does not start a service or
connect any account by itself.
