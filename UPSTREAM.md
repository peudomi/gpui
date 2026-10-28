# Upstream merge policy

This repository is a fork of `zed-industries/zed`, trimmed down to GPUI. It does
not track the whole upstream tree; **only GPUI-related changes are brought in.**

It also carries a second upstream: `crates/gpui_base` is the `gpui-base` crate
of `longbridge/gpui-component`, followed through a vendor branch. See
[gpui-base](#gpui-base).

## Branch structure

`main` is the trunk of this fork. There is no mirror branch for Zed. To refer
to the upstream state, use the `upstream/main` remote-tracking ref after
`git fetch upstream`.

`vendor/gpui-component` is gpui-component's own history, reduced to
`crates/base` and never edited by hand. `main` merges it. See
[gpui-base](#gpui-base).

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
   sections, and accept dependency entries upstream added or updated, except
   the entries [gpui-base](#gpui-base) owns.
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

## gpui-base

`crates/gpui_base` is `crates/base` of
[`longbridge/gpui-component`](https://github.com/longbridge/gpui-component).
Upstream builds it against the `gpui-pre-*` snapshots on crates.io; here it
builds against the crates of this repository. Nothing else from gpui-component
is brought in.

Upstream's commits are kept as they are, with their original hashes:

```
kit-upstream/main ──merge──▶ vendor/gpui-component ──merge──▶ main
(gpui-component)             (only crates/base)               (crates/gpui_base)
```

- `vendor/gpui-component` starts from an upstream commit with one commit that
  removes everything but `crates/base`. Each update merges `kit-upstream/main`
  into it and removes again whatever upstream changed or added outside
  `crates/base`. Nothing else is ever committed to it, so it never conflicts
  inside `crates/base`.
- `main` merged it once and moved `crates/base` to `crates/gpui_base` in that
  merge. Later merges carry upstream's edits to existing files through git's
  rename detection. Files upstream *adds* still arrive under `crates/base` and
  are moved by hand. The fork changes below are ordinary commits on `main`.
- `git blame` on `crates/gpui_base` reaches upstream's commits. A path-limited
  `git log -- crates/gpui_base/...` stops at the first merge, as with any move;
  use `git log --follow` for one file.

### Fork changes in `crates/gpui_base`

Show them with `git diff vendor/gpui-component:crates/base HEAD:crates/gpui_base`.
A merge must keep all of them:

1. **Rounded content masks.** Every `gpui::ContentMask` struct literal needs
   `..Default::default()` for `corner_radii`.
2. **GPUI API drift.** This repository follows Zed `main` daily, so it can be
   ahead of the snapshot upstream compiles against. Minimal source adaptations
   for those differences live here. Prefer the shape Zed's own callers use for
   the new API.
3. **Closed dock extent hook.** `DockAreaRenderer::closed_dock_extent` in
   `src/dock/dock_area.rs` lets a renderer decide how much room a closed dock
   keeps. Its default returns `CLOSED_BOTTOM_STRIP` for the bottom dock and zero
   otherwise, so upstream's behavior is unchanged. Base reads it in
   `render_dock` for a closed dock, and in `resize_dock` and `end_dock_resize`
   where upstream uses `CLOSED_BOTTOM_STRIP`. A dock may be dragged below its
   minimum and shut only when its closed extent is above zero (upstream: only
   the bottom dock), since a dock that keeps nothing also loses the handle that
   would drag it open again. If upstream adds another use of
   `CLOSED_BOTTOM_STRIP` in the resize or render path, route it through the hook
   too.

`crates/gpui_base/Cargo.toml` is never edited. It names its dependencies with
`workspace = true`, and the root `Cargo.toml` provides those keys:

- `gpui`, `gpui_macros`, `gpui_platform`, `reqwest_client`: the crates of this
  repository, already in `[workspace.dependencies]`.
- `lsp-types`, `ropey`, `instant`, `sum-tree`, `gpui-fast`,
  `gpui-fast-platform`: copied from gpui-component's root `Cargo.toml`
  (`git show kit-upstream/main:Cargo.toml`). `lsp-types` replaces Zed's git
  fork, which `gpui-base` does not compile against and no other crate here
  uses. When a Zed merge changes one of these keys, keep ours.
- Every other key it uses is shared with the Zed entries.

When upstream starts using a workspace key the root does not have, add it with
upstream's value. If the key names a `gpui-pre-*` package that this repository
has a crate for, point it at that crate instead.

### Merging gpui-base

Run `script/sync-gpui-base` on a clean checkout of the branch to merge into. It
adds the `kit-upstream` remote if missing, and then:

1. updates `vendor/gpui-component` from `origin` if `origin` is ahead,
2. merges `kit-upstream/main` into it in a temporary worktree, removes every
   path outside `crates/base`, and commits with the default message,
3. merges `vendor/gpui-component` into the current branch, moves files that
   upstream added under `crates/base` to `crates/gpui_base`, and commits with
   the default message if nothing conflicts.

It exits 0 when the merge is committed or there was nothing new, 2 when
conflicts in `crates/gpui_base` are left with the merge in progress, and 1 on
any other error. On 2, resolve the listed files, `git add` them, and
`git commit --no-edit`. Push `vendor/gpui-component` together with the branch.

Both histories must be complete (no shallow fetch). If upstream moves
`crates/base`, stop and ask the owner.

- Conflicts on the fork changes above: take upstream's side, then reapply the
  fork change.
- A conflict reported under `crates/base` instead of `crates/gpui_base` means
  rename detection lost a file the fork changed heavily. Resolve it into
  `crates/gpui_base` and leave nothing under `crates/base`.
- `Cargo.lock`: let the next build rewrite it.
- Compile errors: a missing `corner_radii` field is fork change 1, a missing
  workspace key is the manifest rule above, anything else from GPUI is fork
  change 2.

Verification is the [post-merge verification](#post-merge-verification-mandatory)
above (`cargo check --workspace` builds `gpui-base`), plus
`cargo check -p gpui-base --target aarch64-apple-darwin` and
`--target aarch64-apple-ios`, which need no Apple SDK.

## Automated daily sync

A scheduled Claude Code cloud agent runs this once a day in a Linux sandbox.

1. **Skip when nothing relevant changed.** Let `BASE=$(git merge-base HEAD upstream/main)`.
   If `git diff --quiet $BASE upstream/main -- $(git ls-tree -d --name-only HEAD crates/) 'crates/gpui*' tooling/perf rust-toolchain.toml`
   succeeds, stop without merging.
2. **Merge** with the procedure above on a work branch. Keep the default merge
   message (`Merge remote-tracking branch 'upstream/main'`). New upstream
   `crates/gpui*` crates are kept and added to `members`/`workspace.dependencies`
   only if they and all their in-repo dependencies are Apache-2.0.
3. **Verify** with the mandatory checks above (Linux), then type-check the
   Apple crates without any Apple SDK:
   `cargo check -p gpui_macos --target aarch64-apple-darwin --features runtime_shaders`
   and the same for `gpui_ios` / `aarch64-apple-ios`. `gpui_apple/build.rs`
   gates itself on the *host* OS, so on Linux it emits nothing and the check
   fails on `stitched_shaders.metal`. For the check only, edit the working tree
   so `main` tests `CARGO_CFG_TARGET_OS` for `macos`/`ios` at runtime instead
   of the `#[cfg]`s, and `cbindgen` becomes a plain `[build-dependencies]`
   entry; discard that edit afterwards, never commit it. Errors there are real
   failures. Also try `x86_64-pc-windows-msvc` on `gpui_windows`; record a
   failure caused by a missing Windows build tool as *unverified*.
4. **Land on `main` directly** only if all of these hold:
   - no content conflict (`UU`/`AA`) outside the root `Cargo.toml`/`Cargo.lock`,
     and no hand edit to source beyond the mechanical steps of the merge
     procedure (e.g. fixing code that auto-merged but no longer compiles),
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

The same run then follows gpui-component, on top of `main` as it stands after
the steps above (never on the review branch):

1. **Merge** with `script/sync-gpui-base` on a work branch
   ([Merging gpui-base](#merging-gpui-base)). If it reports both steps up to
   date, stop. On exit 2, resolve the conflicts as described there; fixes made
   during the merge go into the merge commit.
2. **Push `vendor/gpui-component`** to `origin` (a non-force fast-forward)
   whatever happens next, so the next run starts from it.
3. **Verify** with the checks listed there, using the same temporary Apple
   build edit as step 3 above for the Apple targets.
4. **Land on `main` directly** only if all of these hold:
   - `script/sync-gpui-base` exited 0, or its only conflicts were
     `ContentMask` literals resolved as fork change 1,
   - no hand edit beyond fork change 1 and new workspace keys copied from
     upstream (fork change 2 needs review),
   - every check passes.
5. **Otherwise ask for review** as in step 5 above, on
   `claude/upstream-sync-<YYYY-MM-DD>-gpui-base`.
