# CI build optimization

Cache compiled Cargo dependencies first. The first `cargo test --locked` in the baseline `main` run spent several minutes preparing the tests, which then ran in under five seconds per platform. The final `cargo build --locked` completed in under one second on each runner. Keep the explicit build for the existing packaging workflow. [Baseline run][baseline] · [Build workflow][workflow] · [Cargo test target selection][cargo-test]

| Runner | Cargo test build phase | mise setup |
| --- | ---: | ---: |
| Ubuntu 24.04 x64 | 3m 21s | 14s |
| Windows 2022 x64 | 5m 23s | 58s |
| macOS 15 arm64 | 5m 30s | 18s |

## Dependency cache

Use Rust Cache v2.9.3, which supports the intermediate build directory layout introduced in Cargo 1.100. Run it after mise selects Rust and the platform-specific setup exports its compiler environment. The action keys on OS, architecture, Rust compiler identity, Cargo configuration and dependencies, and compiler environment such as Linux `CC` and `CXX`. Add `DEVELOPER_DIR` to the environment key for macOS and route the action’s Cargo and Rust probes through `mise exec`. [Cargo changelog][cargo-layout] · [Rust Cache layout support][rust-cache-layout] · [Rust Cache configuration][rust-cache-config]

Cache the Cargo registry and compiled dependency outputs with binary caching disabled. The action excludes workspace build outputs by default and disables incremental compilation for the job. Cargo still builds the app with the current deployment endpoints, which its build script tracks explicitly. Save on successful `main` pushes so PRs can reuse a shared cache. PR cache entries would be scoped to their individual merge refs. [Rust Cache details][rust-cache-details] · [App build script][build-script] · [GitHub cache scopes][cache-scopes]

## Measure on hosted runners

1. Record the first cold `main` run that saves the three caches. Compare it with a warm run using the same commit or an app-only change with identical toolchains, dependencies, and build configuration.
2. Record each job’s elapsed time, including cache restore and post-job save, alongside the Cargo test build phase and compressed archive size. Check the next PR restores the `main` cache and uses the compiled dependencies. Confirm the platform matrix, native fixtures, and packaging checks pass.
3. Keep the cache where it improves total job time. Watch for eviction if successive dependency updates create large archives. GitHub defaults to 10 GB per repo and removes entries that go unused for over seven days. [GitHub cache limits][cache-limits]

## Other options

- The mise cache hit on all three baseline runners, and `rustup` still downloaded the pinned toolchain. If toolchain setup dominates after the dependency cache warms, measure a separate toolchain cache against the restore cost, starting with Windows. [Baseline run][baseline] · [mise Rust caching][mise-cache]
- Consider `sccache` if archive transfer or eviction makes dependency caching ineffective. Its Rust cache excludes compilations that invoke the system linker, including binaries and procedural macros. [sccache Rust support][sccache]
- Cancel superseded runs within each PR if rapid pushes keep overlapping. The two observed PR runs overlapped for about five minutes. PR cancellation saves runner time, while the current run still takes as long to build. [Earlier run][earlier-pr] · [Later run][later-pr] · [GitHub concurrency][concurrency]

[baseline]: https://github.com/ChristopheCVB/spk-gpui/actions/runs/44949070885
[workflow]: ../../.github/workflows/build.yml
[cargo-test]: https://doc.rust-lang.org/cargo/commands/cargo-test.html#target-selection
[cargo-layout]: https://doc.rust-lang.org/cargo/CHANGELOG.html#cargo-1100-2026-11-12
[rust-cache-layout]: https://github.com/Swatinem/rust-cache/releases/tag/v2.9.2
[rust-cache-config]: https://github.com/Swatinem/rust-cache/blob/v2.9.3/src/config.ts
[rust-cache-details]: https://github.com/Swatinem/rust-cache/blob/v2.9.3/README.md#cache-details
[build-script]: ../../build.rs
[cache-scopes]: https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching#restrictions-for-accessing-a-cache
[cache-limits]: https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching#usage-limits-and-eviction-policy
[mise-cache]: https://github.com/jdx/mise-action#rust-cache
[sccache]: https://github.com/mozilla/sccache/blob/main/docs/Rust.md
[earlier-pr]: https://github.com/ChristopheCVB/spk-gpui/actions/runs/44939123709
[later-pr]: https://github.com/ChristopheCVB/spk-gpui/actions/runs/44939529565
[concurrency]: https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/control-workflow-concurrency
