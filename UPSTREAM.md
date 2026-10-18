# Upstream merge policy

This repository is a fork of `zed-industries/zed`, trimmed down to GPUI. It does
not track the whole upstream tree; **only GPUI-related changes are brought in.**

## Branch structure

`main` is the trunk of this fork. There is no separate mirror branch. To refer
to the upstream state, use the `upstream/main` remote-tracking ref after
`git fetch upstream`.

## What to bring in

- `main` is merged with `upstream/main` **daily** by a scheduled cloud agent
  (see [Automated daily sync](#automated-daily-sync)) so drift never piles up.
  Manual merges follow the same procedure.
- If you only need a single small fix, prefer `git cherry-pick -x <sha>` over a
  merge (`-x` records the source sha in the commit message).
- Only changes touching the remaining crates (`crates/*`, `tooling/perf`) are
  relevant. Everything else resolves as a deletion anyway.

## Merge procedure

```shell
git fetch upstream
git merge upstream/main
```

Conflicts fall into three categories:

1. **Modify/delete conflicts on removed paths** (the vast majority)
   Upstream modified a file we deleted. Delete them all again:

   ```shell
   git status --porcelain | awk '$1 == "DU" { print $2 }' | xargs -r git rm -q --
   ```

   Files that upstream *added* under a removed path do not conflict: they
   arrive staged as `A` and must be removed too. Keep only additions under the
   remaining crates:

   ```shell
   git status --porcelain | awk '$1 == "A" { print $2 }' \
     | grep -Ev '^(crates/(bench_metrics|collections|gpui[a-z_]*|http_client|http_client_tls|path|refineable|reqwest_client|scheduler|sum_tree|util|util_macros)|tooling/perf)/' \
     | xargs -r git rm -q -f --
   ```

   Upstream can also delete a crate we keep (`media` was replaced by `objc2`
   bindings in `4b47ceb9d3`); follow upstream and drop it from `members`.

2. **Root `Cargo.toml` / `Cargo.lock`**
   Resolve `Cargo.toml` by hand: keep our trimmed members/default-members/patch
   sections, and accept dependency entries upstream added or updated.
   For `Cargo.lock`, run `git checkout --ours Cargo.lock` and let the next build
   rewrite it.

3. **GPL remnants reappearing**
   Upstream keeps using its GPL crates (`ztracing`, `ztracing_macro`, `zlog`),
   so they may come back with every merge.
   - If a directory such as `crates/ztracing` is resurrected, delete it again.
   - New `#[ztracing::instrument]` call sites surface as compile errors. Remove
     the attributes and the `use ztracing::instrument;` lines, then drop the
     `ztracing`/`tracing` dependencies and the `cargo-machete` ignore entries
     from the affected crate's `Cargo.toml`.
   - `zlog` was only used as a test logger hook; delete the hook block.

## When upstream adds a new in-repo dependency

If GPUI starts depending on a new crate inside the monorepo:

1. Check the crate's `license` field and `LICENSE-*` files. **Only Apache-2.0 is
   allowed.** If it is GPL-family, do not bring it in; strip the call sites or
   write a replacement, as was done for `ztracing`.
2. If it is Apache, restore it with `git checkout upstream/main -- crates/<name>`
   and add it to `members` and `workspace.dependencies` in the root `Cargo.toml`.

## Post-merge verification (mandatory)

```shell
cargo check --workspace
cargo test -p sum_tree -p gpui
script/check-licenses        # full license audit, see below
```

`script/check-licenses` verifies that every local crate is Apache-2.0 and that
the dependency graph contains no unavoidable copyleft (GPL/AGPL and similar with
no permissive OR-alternative); it fails on any violation. Also confirm that the
modification notice in the README (§4(b)) still covers the new changes.

## What not to do

- Do not put app-specific hacks into this repository that upstream does not
  have. Patches must be general-purpose features.
- Do not keep GPL code around "to clean up later". It must be zero lines at
  every merge commit.

## Automated daily sync

A scheduled Claude Code cloud agent runs this once a day in a Linux sandbox.

1. **Skip when nothing relevant changed.** Let `BASE=$(git merge-base HEAD upstream/main)`.
   If `git diff --quiet $BASE upstream/main -- $(git ls-tree -d --name-only HEAD crates/) 'crates/gpui*' tooling/perf rust-toolchain.toml`
   succeeds, stop without merging.
2. **Merge** with the procedure above on a work branch. Keep the default merge
   message (`Merge remote-tracking branch 'upstream/main'`). New upstream
   `crates/gpui*` crates are kept and added to `members`/`workspace.dependencies`
   only if they and all their in-repo dependencies are Apache-2.0.
3. **Verify** with the mandatory checks above (Linux). macOS, iOS and Windows
   code cannot be fully compiled there; try
   `cargo check --target <triple>` for `aarch64-apple-darwin`,
   `aarch64-apple-ios` and `x86_64-pc-windows-msvc` on the platform crates and
   record a failure caused by a missing SDK or build tool as *unverified*, not
   as an error.
4. **Land on `main` directly** only if all of these hold:
   - no content conflict (`UU`/`AA`) outside the root `Cargo.toml`/`Cargo.lock`,
   - every Linux check passes,
   - no license decision was needed (no new non-`gpui*` in-repo crate, no new
     copyleft dependency, no GPL removal beyond deleting resurrected directories
     and `ztracing` attributes),
   - no file the fork modified (`git diff --name-only --diff-filter=M $BASE HEAD -- crates tooling`)
     was also changed upstream (`git diff --name-only $BASE upstream/main`) in
     a platform crate that could not be compiled in step 3.
5. **Otherwise ask for review**: commit the merge as far as it got, push it to
   `claude/upstream-sync-<YYYY-MM-DD>`, leave `main` untouched, and notify the
   owner. The run's final report (Korean) lists the upstream range, each
   conflict and how it was resolved, failing or unverified checks, and notes
   for the fork change log in `studio-docs/gpui/`.
