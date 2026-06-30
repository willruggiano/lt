# lt

A local-first [Linear] TUI for terminal power users.

![It's not much... but it's mine :)](https://github.com/user-attachments/assets/bb8c14df-b1b2-45d5-a366-85f21a2a0d3f)

Reads are instant; queries use a local SQLite cache, not the network.

Writes are optimistic and queued offline.

The search bar speaks a structured query language whose parser is **generated at
build time** from Linear's GraphQL schema, so filter keys stay in sync with the
API automatically.

> [!WARNING] This is a side project. I started it as a means of experimenting
> with coding agents. Expect rough edges!

## Features

- **Local-first**: GraphQL polling + SQLite cache. Instant reads, optimistic
  writes, offline queue.
- **Structured search**: `assignee:me priority:urgent sort:updated-` and
  free-text, parsed by a [Chumsky]-based parser generated from Linear's
  [canonical GraphQL schema](./build/linear-schema-definition.graphql) at
  `cargo build` time.
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

The redirect URI is: <http://localhost:7342/callback>

Then configure the relevant environment variables:

```bash
export LINEAR_CLIENT_ID=
export LINEAR_CLIENT_SECRET=
```

Application state is kept per profile in `$XDG_STATE_HOME/lt/profiles/<name>`
(`~/.local/state/lt/profiles/default` on Linux when no profile is selected):

- `auth.json` contains OAuth credentials (0600 permissions)
- logs are timestamped in the `logs/` sub-directory

### Profiles (multiple accounts / workspaces)

Each profile has its own credentials and local database, so different accounts
or workspaces never share state and can run side by side:

```bash
lt --profile work auth login   # authenticate the "work" profile
lt --profile work              # TUI for the work account
LT_PROFILE=personal lt         # env var alternative
```

When no profile is given, the profile named `default` is used. The OAuth
application credentials (`LINEAR_CLIENT_ID`/`LINEAR_CLIENT_SECRET` or the stored
config file) are shared across profiles.

## Why not Linear's sync protocol?

TLDR: it is a private API.

Linear's web client uses an internal sync engine (`client-api.linear.app`) that
streams delta packets over WebSocket. The protocol is well reverse-engineered at
[wzhudev/reverse-linear-sync-engine], but the bootstrap endpoint rejects OAuth
tokens and personal API keys - it is browser-session-only by design.

The practical gap is small for a single-user TUI: reads are instant (local
cache), writes are optimistic, and remote changes arrive on the next poll cycle.

## Reference

- <https://linear.app/developers/graphql>
- <https://studio.apollographql.com/public/Linear-API/variant/current/home>

[Chumsky]: https://github.com/zesterer/chumsky
[Linear]: https://linear.app
[ratatui]: https://github.com/ratatui/ratatui
[rusqlite]: https://github.com/rusqlite/rusqlite
[ureq]: https://github.com/algesten/ureq
[wzhudev/reverse-linear-sync-engine]:
  https://github.com/wzhudev/reverse-linear-sync-engine
