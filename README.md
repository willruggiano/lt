# lt

A local-first [Linear] TUI for terminal power users.

Reads are instant; queries use a local SQLite cache, not the network.

Writes are optimistic and queued offline.

The search bar speaks a structured query language whose parser is **generated at build time** from
Linear's GraphQL schema, so filter keys stay in sync with the API automatically.

> [!WARNING]
> This is a side project. I started it as a means of experimenting with coding agents. Expect rough edges!

## Features

- **Local-first**: GraphQL polling + SQLite cache. Instant reads, optimistic writes, offline queue.
- **Structured search**: `assignee:me priority:urgent sort:updated-` and
  free-text, parsed by a [Chumsky]-based parser generated from Linear's
  [canonical GraphQL schema](./docs/reference/linear-schema-definition.graphql) at `cargo build` time.
- **Vim-style keybindings**
- **Built with Rust** - [ratatui], [rusqlite], [ureq].

### Search query syntax

Tokens are whitespace-separated. A `key:value` token is a filter stem; bare
words become prefix-matched full-text search terms.

| Stem        | Example           | Notes                               |
| ----------- | ----------------- | ----------------------------------- |
| `assignee:` | `assignee:will`   | filter by assignee name             |
| `priority:` | `priority:urgent` | urgent / high / normal / low / none |
| `state:`    | `state:todo`      | workflow state name                 |
| `team:`     | `team:backend`    | team name                           |
| `label:`    | `label:bug`       | issue label                         |
| `project:`  | `project:v2`      | project name                        |
| `cycle:`    | `cycle:current`   | cycle name                          |
| `creator:`  | `creator:alice`   | issue creator                       |
| `sort:`     | `sort:updated-`   | field + `+`/`-` for asc/desc        |

Free-text words are FTS5 prefix-searched against issue identifier and title.
Unknown keys get a "did you mean?" suggestion via edit distance.

## Install

### Cargo

Requires a nightly Rust toolchain (edition 2024).

```bash
cargo install -git https://github.com/willruggiano/lt
```

### Nix

```bash
# run directly
nix run github:willruggiano/lt
```

or by adding `github:willruggiano/lt` as an input to your own Nix flake.

## Usage

You will need to create a Linear OAuth application following Linear's official
documentation: <https://linear.app/developers/oauth-2-0-authentication>

Then configure the relevant environment variables:

```bash
export LINEAR_CLIENT_ID=
export LINEAR_CLIENT_SECRET=
```

Application state is kept in XDG_STATE_HOME/lt (`~/.local/state/lt` on Linux):

- `auth.json` contains OAuth credentials (0600 permissions)
- logs are timestamped in the `logs/` sub-directory

## Why not Linear's sync protocol?

TLDR: it is a private API.

Linear's web client uses an internal sync engine (`client-api.linear.app`) that
streams delta packets over WebSocket. The protocol is well reverse-engineered at
[wzhudev/reverse-linear-sync-engine], but the bootstrap endpoint rejects OAuth
tokens and personal API keys - it is browser-session-only by design.

The practical gap is small for a single-user TUI: reads are instant (local
cache), writes are optimistic, and remote changes arrive on the next poll cycle.

## Reference

- [./docs/reference/linear-schema-definition.graphql]
- <https://linear.app/developers/graphql>
- [wzhudev/reverse-linear-sync-engine]

[Chumsky]: https://github.com/zesterer/chumsky
[Linear]: https://linear.app
[ratatui]: https://github.com/ratatui/ratatui
[rusqlite]: https://github.com/rusqlite/rusqlite
[ureq]: https://github.com/algesten/ureq
[wzhudev/reverse-linear-sync-engine]: https://github.com/wzhudev/reverse-linear-sync-engine
