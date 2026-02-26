# lt

The Linear tui for terminal power users.

## Features:

- Local-first: GraphQL polling with a local SQLite cache for instant reads and
  offline-queued writes.
- Vim-like keybindings.
- Built with Rust and ratatui

## Architecture note: why not Linear's sync protocol?

Linear's web client uses an internal sync engine at `client-api.linear.app`
that streams delta packets over WebSocket and supports true real-time local-first
sync. The protocol is well reverse-engineered at [wzhudev/reverse-linear-sync-engine].

We tested it empirically: the bootstrap endpoint rejects both OAuth tokens and
personal API keys with explicit auth-method errors. The endpoint is
browser-session-only by design and is not accessible to programmatic clients.

The practical gap between polling and true sync is small for a single-user TUI:
reads are always instant (local cache), writes are optimistic, and remote changes
from teammates arrive on the next poll cycle.

## Reference:

- https://linear.app/developers/oauth-2-0-authentication
- [./docs/reference/linear-schema-definition.graphql]
- https://linear.app/developers/graphql
- [wzhudev/reverse-linear-sync-engine]

[wzhudev/reverse-linear-sync-engine]: https://github.com/wzhudev/reverse-linear-sync-engine
