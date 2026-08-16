# rust-sdk-runtime

The module runtime that Rust SDK modules reference. It is **build-only**: it is
not something you call, and it holds no SDK surface of its own.

New modules created with `dagger module init rust` get a `dagger-module.toml`
pointing at this module:

```toml
[runtime]
  source = "github.com/wingyplus/rust-sdk/runtime"
```

Rust has no built-in engine runtime, so the engine loads this module to learn
how to build and start a Rust module. That is the whole job:

- `codegen` is a **no-op**. Modules commit their generated `dagger_sdk/`
  directory, so there is nothing to generate at module load. Generation is
  owned by `dagger generate` in the [parent SDK module](../mod.dang).
- `moduleRuntime` runs `cargo build --release` over the committed sources,
  copies the resulting binary to `/usr/local/bin/dagger-module`, drops the cargo
  cache mounts, and sets that binary as the entrypoint.

Because a goish binary is statically linked with no libc and no dynamic loader,
the entrypoint is a single self-contained file. (Serving it from a `scratch`
base rather than the build image is an obvious follow-up.)

## Invariants worth knowing

- **The binary name is derived, not configured.** `toRustCrateName` in
  `main.dang` turns the Dagger module name into the cargo package name, which is
  the filename cargo emits. It must stay byte-for-byte identical to
  `rust_crate_name` in [`helpers/render-template/src/lib.rs`](../helpers/render-template/src/lib.rs),
  which writes that same name into the scaffolded `Cargo.toml`. If the two
  diverge, the build succeeds and the entrypoint points at a path that does not
  exist.
- **The target tuple is fixed** to `x86_64-unknown-linux-gnu`, matching goish's
  supported platform and the `[build] target` in each module's
  `.cargo/config.toml`. Naming the target explicitly is what keeps the
  bare-metal link flags off host-built proc-macro crates.
- **A module missing `dagger_sdk/` fails fast** with a message naming
  `dagger generate`, rather than letting cargo report an unresolvable path
  dependency.
