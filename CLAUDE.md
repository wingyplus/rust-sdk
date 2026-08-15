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

The one non-Rust exception is `helpers/render-template/`, a small Go program
that renders init templates. It runs at init time in a golang container and has
nothing to do with the module runtime.

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
