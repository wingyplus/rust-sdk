# rust-sdk

The Dagger Rust SDK: a Dang module that authors and generates Rust Dagger
modules, ported from [`github.com/dagger/go-sdk`](https://github.com/dagger/go-sdk).
See [README.md](./README.md) for the user-facing picture and current status.

## The rule: all Rust here is `no_std` on goish

**Every Rust crate in this repository is `#![no_std]` and depends on
[goish](https://github.com/cogentica-ai/goish).** There is no exception — not
the client library, not the bindings generator, not the code the templates
scaffold. Do not introduce a `std` crate, and do not reach for crates.io.

goish is a `no_std` reimplementation of Go's standard library and runtime in
Rust. Two reasons it is the whole foundation here:

1. **The port is mechanical.** This SDK is a port of the Go SDK. goish gives
   Rust the same `net/http`, `crypto/tls` and `encoding/json` the Go client is
   written against, so the client can be translated rather than redesigned.
2. **The output is a single static binary.** No libc, no dynamic loader, no GC.
   A module image is one self-contained file.

Practical consequences when writing code here:

- Reach for `goish::os` where you would reach for `std::fs`/`std::env`,
  `goish::fmt::Sprintf!` where you would reach for `format!`, and
  `goish::encoding::json` where you would reach for serde. Use goish's
  Go-shaped types — `string`, `slice<T>`, `map<K, V>` — not `String`/`Vec`.
- Every binary crate needs `#![no_std]`, `#![no_main]`, and a `#[goish::main]`
  entry point.
- Every crate needs a `.cargo/config.toml` with goish's link flags
  (`-nostartfiles -nodefaultlibs -static`, `relocation-model=static`,
  `force-frame-pointers=yes`) and an explicit `[build] target`. Without the
  flags a goish binary does not link; without the explicit target the flags leak
  into host-built proc-macro crates and break PIC.
- Because `[build] target` is set, binaries land at
  `$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/release/<name>`, **not**
  `release/<name>`. `mod.dang` and `runtime/main.dang` both depend on this path.
- `panic = "abort"` in both profiles. goish never unwinds.
- goish is Linux x86_64 only — its runtime is x86_64 inline asm — so
  `cargo target` is always `x86_64-unknown-linux-gnu` and any container that
  *runs* a goish binary is created with `container(platform: servePlatform)`,
  i.e. `linux/amd64`. Building is the opposite: build containers follow the
  engine's own platform and cross-link, so rustc runs natively rather than
  under emulation. See "Building is cross, running is amd64" below.

The one non-Rust exception is `helpers/render-template/`, a small Go program
that renders init templates. It runs at init time in a golang container and has
nothing to do with the module runtime.

## Building is cross, running is amd64

Two different questions, and conflating them is the mistake to avoid:

- **What tuple is the binary?** Always `x86_64-unknown-linux-gnu`. goish has no
  aarch64 port — it has zero `cfg(target_arch)` gates and ~24 raw x86_64 `asm!`
  blocks, and compiling it for aarch64 dies with 41 errors of the form
  ``the `att_syntax` option is only supported on x86``. This is not negotiable
  until goish itself gains a port, which is upstream work in
  `cogentica-ai/goish`, not something this repo can do.
- **What platform does the build run on?** The engine's own. `container()` with
  no `platform:` argument in `mod.dang` and `runtime/main.dang` follows the
  engine, and cross-compiles to the tuple above.

rustc cross-compiles happily once `rustup target add x86_64-unknown-linux-gnu`
has run — the arm64 `rust` image *does* have an x86_64 `core`, contrary to what
an earlier revision of these docs claimed. The single thing that breaks is the
link step: rustc shells out to `cc` with `-m64`, and the stock arm64 driver
answers `unrecognized command-line option '-m64'`. `crossToolchainSetup`
installs `gcc-x86-64-linux-gnu` when `uname -m` is not `x86_64`, and
`crossLinkerEnv` points cargo at `x86_64-linux-gnu-gcc` — a name Debian uses for
the *native* driver on an amd64 host, so one value is correct on both.

That override is set **by environment, never in `.cargo/config.toml`**. Every
module commits that file and `sdk/codegen` has its own; they must keep working
byte-for-byte unchanged, which is what keeps this change clear of the
`rustCrateName`/`toRustCrateName` invariant entirely.

The finished binary is then served from a bare `container(platform:
servePlatform)` with nothing in it but the binary — goish links statically with
no libc and no dynamic loader, so nothing else is needed. Serving on amd64 is
still mandatory: an x86_64 binary cannot be `exec`'d as the entrypoint of an
arm64 container. Only that one small binary runs emulated; the compile does not.

Cache volumes holding compiled objects (`rust-sdk-module-target-*`,
`rust-sdk-codegen-target-*`) are suffixed with `buildHostKey` because cargo puts
host build scripts and proc-macro `.so`s under `$CARGO_TARGET_DIR/release/`,
which an engine of the other architecture cannot exec. The registry and git
caches hold source, not objects, and stay shared.

## Invariants that will bite you

- **The goish git pin must match across crates.** goish is not on crates.io, so
  it is pinned by rev in `sdk/Cargo.toml`, `sdk/codegen/Cargo.toml` and every
  `templates/*/Cargo.toml.tmpl`. Cargo treats two revs as two different crates:
  if a module and its vendored `dagger_sdk` disagree, the link fails on
  duplicate runtime symbols. Bump them together.
- **Two name derivations must stay byte-for-byte identical.** `rustCrateName` in
  `helpers/render-template/main.go` writes the cargo `[package]` name at init
  time; `toRustCrateName` in `runtime/main.dang` recomputes it at call time to
  locate the built binary. A divergence builds fine and then fails to start.
  `TestNameConversionsMatchDang` guards the cases a general-purpose case library
  gets wrong (`HTTPServer` → `http_server`, not `httpserver`).
- **Cargo caches git dependencies under `$CARGO_HOME/git`, not
  `$CARGO_HOME/registry`.** goish is the only real dependency and it is a git
  dep, so caching just the registry re-clones it on every build. Both mounts
  appear in `mod.dang` and `runtime/main.dang`.
- **Reserved-identifier checking is asymmetric on purpose.** The derived *type*
  name is rejected when it is a Rust keyword, because it becomes a `struct`
  declaration and `pub struct Self;` will not parse. The derived *crate* name is
  not, because it is only ever a cargo package name, a bin target name and a
  filename — a module named `crate` renders, builds and runs. (`cargo new`
  refuses such names only because it also creates a lib target, which these
  templates do not.) Both halves are pinned by tests; don't "fix" the crate side.
- **`initModule` seeds `dagger_sdk/`, and that is not a convenience.** The engine
  discovers a workspace's generators by *loading* every module in it, and loading
  a Rust module builds it. If a scaffolded module's `Cargo.toml` named a
  `dagger_sdk` that did not exist yet, it could not build, so it could not load,
  so `dagger generate` could not enumerate what to generate — generation would
  have to have already run for generation to be possible. Seeding at init breaks
  that cycle. Don't move it into `generate`.
- **Top-level `let` is file-scoped in Dang, and list literals need a type
  annotation.** `let xs = ["a"]` fails to resolve with a bare "not found"; write
  `let xs: [String!]! = ["a"]`. Constants shared between `rust-sdk.dang` and
  `mod.dang` are therefore duplicated and marked keep-in-step, the same way
  `rustImage` is between `mod.dang` and `runtime/main.dang`.
- **The hand-written object types must stay out of `src/gen`.** `Changeset` and
  `Workspace` in `sdk/src/objects.rs` are the only object types a module can
  name today — the generator contract needs them — and they are deliberately at
  the crate root. `dagger generate` replaces `src/gen/` wholesale, so moving
  them there would delete them from every generated module: a module with a
  `#[dagger::function(generate)]` would compile until the first `generate` and
  never again.
- **The engine owns module config.** `initModule` must not write `dagger.json`
  or `dagger-module.toml`; it returns only SDK-owned files. The engine merges in
  its own bookkeeping.

## Layout

| Path | What it is |
| --- | --- |
| `rust-sdk.dang` | SDK contract: `initModule`, `targetRuntime`, `modules`, `mod`, `@generate` |
| `mod.dang` | A managed module: vendors the SDK + generated bindings into `dagger_sdk/` |
| `template.dang` | Init template value type |
| `runtime/` | Build-only module runtime new Rust modules point at |
| `sdk/` | The `dagger-sdk` crate and `sdk/codegen`, its bindings generator |
| `templates/` | Starters for `dagger module init rust` |
| `helpers/render-template/` | Go helper that renders a template for a module name |

## Working on this repo

Dang sources: `rust-sdk.dang`, `mod.dang`, `template.dang`, `runtime/main.dang`.
Format with `dang fmt -w`, but check the binary first — a `dang` older than the
`pub` keyword will silently strip `pub` from every declaration.

Verify the Go helper directly:

```sh
cd helpers/render-template && go test ./...
```

Run the shared SDK contract suite:

```sh
dagger -m github.com/dagger/sdk-sdk -W . check
```

To test a Rust change without an engine, build the crate against a local goish
checkout — swap the git dep for a path dep in a scratch copy rather than editing
the committed manifest.
