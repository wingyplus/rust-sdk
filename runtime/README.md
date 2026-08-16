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
- `moduleRuntime` runs `cargo build --release` over the committed sources in a
  container on the engine's own platform, then copies the resulting binary into
  a fresh `linux/amd64` container and sets it as the entrypoint.

Because a goish binary is statically linked with no libc and no dynamic loader,
the entrypoint is a single self-contained file, and the container serving it is
a fresh one carrying nothing but that binary — no cargo caches, no source, no
target directory — on a small `alpine` base. `scratch` would be the natural end
point and the module binary does run there, but it breaks two sdk-sdk contract
checks; see the table in the root [CLAUDE.md](../CLAUDE.md) before changing the
base.

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
- **The build follows the engine; the serve container does not.** goish has no
  aarch64 port, so the binary is always x86_64 and the container it is served
  from is always `linux/amd64` — an x86_64 binary cannot be `exec`'d as the
  entrypoint of an arm64 container. The *build*, though, runs on the engine's
  own platform and cross-links, so on an arm64 engine rustc runs natively
  instead of under emulation. Only the finished binary is emulated.
  (The arm64 `rust` image does carry an x86_64 `core`, so `rustup target add`
  is enough for rustc; what it lacks is a linker that understands `-m64`, which
  `crossToolchainSetup` installs.)
- **A module missing `dagger_sdk/` fails fast** with a message naming
  `dagger generate`, rather than letting cargo report an unresolvable path
  dependency.
