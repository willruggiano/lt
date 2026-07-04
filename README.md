# lt

> [!WARNING] This is a side project. It has been almost entirely built using
> Claude Code. Note that that is **not** to say that it has been vibe coded. It
> is indeed safe to use, and I use (and improve) it daily.

A local-first [Linear] tui.

![It's not much... but it's mine :)](https://github.com/user-attachments/assets/bb8c14df-b1b2-45d5-a366-85f21a2a0d3f)

## Features

- **Local-first**: instant reads, background sync, optimistic writes
- **Codegen, codegen, codegen**: the GraphQL schema is used to generate Rust
  types, including the _search parser_ which is implemented using [Chumsky]
- **Vim-style keybindings** and zero mouse support: to keep the vibers away
- **Built with Rust**: for the sole reason of learning the language
- **Not vibe coded**: browse the PRs if you don't believe me

## Install

(requires a nightly Rust toolchain)

```bash
cargo install -git https://github.com/willruggiano/lt
```

```bash
nix profile add github:willruggiano/lt
```

```bash
claude "install https://github.com/willruggiano/lt"
```

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
(`/.local/state/lt/profiles/default` on Linux when no profile is selected):

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
[wzhudev/reverse-linear-sync-engine]:
  https://github.com/wzhudev/reverse-linear-sync-engine
