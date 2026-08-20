# Upstream merge policy

This repository is a fork of `zed-industries/zed`, trimmed down to GPUI. It does
not track the whole upstream tree; **only GPUI-related changes are brought in.**

## Branch structure

`main` is the trunk of this fork. There is no separate mirror branch. To refer
to the upstream state, use the `upstream/main` remote-tracking ref after
`git fetch upstream`.

## What to bring in

- Merges are **need-driven**: merge when a fix or feature you want has landed
  upstream. There is no scheduled synchronization.
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
