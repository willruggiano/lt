# Materialize a git-shaped checkout of the pinned RustSec advisory database so
# `cargo deny --offline check` resolves it locally instead of git-cloning
# github.com/rustsec/advisory-db (which 403s behind the repo-scoped git proxy in
# Claude Code web sessions).
#
# cargo-deny (0.19.9) keeps the db under a url-hashed subdir and, even offline,
# needs a real git checkout: a committed tree plus a `.git/FETCH_HEAD` whose
# first field is a commit that exists locally. A plain extracted tree errors with
# "failed to load any advisories"; flake inputs strip `.git`, so we re-init one.
# The subdir name and FETCH_HEAD shape were verified empirically against
# cargo-deny 0.19.9.
#
#   <db-path>/
#     advisory-db-3157b0e258782691/   <- derived by cargo-deny from the db url
#       <advisory tomls...>           <- committed
#       .git/FETCH_HEAD               <- "<local HEAD>\t\tbranch 'main' of <url>"
{
  pkgs,
  advisoryDb,
}:
# Returns a bash snippet that idempotently materializes the database at `dbPath`
# (the deny.toml [advisories] db-path, relative to the cwd cargo-deny runs in).
# The work is wrapped in an errexit subshell so it is safe to source from an
# interactive shell without aborting the caller.
dbPath: ''
  (
    set -eu
    export PATH=${pkgs.git}/bin:${pkgs.coreutils}/bin:''${PATH:-}
    db_root="${dbPath}"
    db_dir="$db_root/advisory-db-3157b0e258782691"
    if [ ! -e "$db_dir/.git/FETCH_HEAD" ]; then
      rm -rf "$db_root"
      mkdir -p "$db_dir"
      cp -r ${advisoryDb}/. "$db_dir/"
      chmod -R u+w "$db_dir"
      export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
      git -C "$db_dir" init -q -b main
      git -C "$db_dir" -c user.email=lt@localhost -c user.name=lt -c commit.gpgsign=false add -A
      git -C "$db_dir" -c user.email=lt@localhost -c user.name=lt -c commit.gpgsign=false commit -qm "vendored rustsec/advisory-db"
      printf "%s\t\tbranch 'main' of https://github.com/rustsec/advisory-db\n" \
        "$(git -C "$db_dir" rev-parse HEAD)" > "$db_dir/.git/FETCH_HEAD"
    fi
  )
''
