# gpui (fork)

This repository is a fork of [GPUI](https://www.gpui.rs), the GPU-accelerated UI
framework for Rust, extracted from the [Zed](https://github.com/zed-industries/zed)
monorepo by Zed Industries, Inc.

## Notice of modifications

Per the Apache License 2.0 §4(b), this fork has been modified from the upstream
source (`zed-industries/zed`):

- Everything unrelated to GPUI has been removed. Only the `gpui*` crates, their
  in-repo dependencies (`collections`, `refineable`, `http_client`,
  `http_client_tls`, `media`, `path`, `reqwest_client`, `scheduler`, `sum_tree`,
  `util`, `util_macros`, `tooling/perf`), and the build configuration remain.
- The upstream `ztracing`, `ztracing_macro`, and `zlog` crates (GPL-3.0-or-later)
  have been removed entirely, along with their few call sites: the
  `#[ztracing::instrument]` profiling attributes in `gpui` and `sum_tree` and a
  test-only logger hook in `sum_tree`.
- The root workspace manifest, `.cargo` config, and repository scaffolding were
  trimmed accordingly.

Further modifications will be tracked in this repository's git history.

## License

Apache License 2.0. See [LICENSE-APACHE](LICENSE-APACHE).

Copyright of the original code remains with Zed Industries, Inc. and the Zed
contributors. This fork contains no GPL-licensed code: every remaining crate is
licensed under Apache-2.0, and third-party dependencies are permissively
licensed (MIT/Apache/BSD/MPL-2.0 and similar).

"Zed" and "GPUI" are trademarks of Zed Industries, Inc.; this fork is not
affiliated with or endorsed by Zed Industries.

## Upstream

The `upstream` remote tracks `zed-industries/zed`. Only GPUI-related changes are
merged from upstream; the merge workflow, conflict-resolution rules, and the
mandatory post-merge license audit (`script/check-licenses`) are documented in
[UPSTREAM.md](UPSTREAM.md).

```shell
cargo check -p gpui        # build the library
cargo run -p gpui --example hello_world
```
