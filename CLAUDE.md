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

There is no exception left: `helpers/render-template/`, which renders init
templates, was a Go program and is now a goish binary too. It runs at init time
in the toolchain image and has nothing to do with the module runtime, but it is
built and tested like every other crate here.

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

The finished binary is then served from a *fresh* container based on `runImage`
(a digest-pinned `alpine`) — no cargo caches, no source, no target directory,
just the binary. Because the binary is statically linked with no libc, the base
supplies nothing it needs and musl vs glibc does not matter; it is picked purely
to be small. Serving on amd64 is mandatory: an x86_64 binary cannot be `exec`'d
as the entrypoint of an arm64 container. Only that one binary runs emulated; the
compile does not.

**`scratch` is the one base that does not work**, which is worth knowing before
anyone tries the obvious optimisation. An empty rootfs fails two sdk-sdk
contract checks, 6 runs out of 6, where any real base passes:

| check | symptom on `scratch` |
| --- | --- |
| `generation:exposes-generator` | nested CLI dies in package init (`bluemonday/css.init()` → `growslice: len out of range`) |
| `contract:honors-custom-path` | `initModule` reports no added paths |

Neither failure appears in a scaffolded module's own init/generate/build/call
path, which passes on `scratch` — so those two checks, not a manual smoke test,
are what to re-run against any change of base. Note also that this suite is
noticeably flaky under emulation: single-check runs repeated a few times are far
more trustworthy than one whole-suite run, and a result seen twice is not yet
evidence of determinism.

Cache volumes holding compiled objects (`rust-sdk-module-target-*`,
`rust-sdk-codegen-target-*`) are suffixed with `buildHostKey` because cargo puts
host build scripts and proc-macro `.so`s under `$CARGO_TARGET_DIR/release/`,
which an engine of the other architecture cannot exec. The registry and git
caches hold source, not objects, and stay shared.

## Invariants that will bite you

- **The goish git pin must match across crates.** goish is not on crates.io, so
  it is pinned by rev in `sdk/Cargo.toml`, `sdk/codegen/Cargo.toml`,
  `helpers/render-template/Cargo.toml` and every `templates/*/Cargo.toml.tmpl`.
  Cargo treats two revs as two different crates: if a module and its vendored
  `dagger` disagree, the link fails on duplicate runtime symbols. Bump them
  together.
- **Two name derivations must stay byte-for-byte identical.** `rust_crate_name`
  in `helpers/render-template/src/lib.rs` writes the cargo `[package]` name at
  init time; `toRustCrateName` in `runtime/main.dang` recomputes it at call time
  to locate the built binary. A divergence builds fine and then fails to start.
  `TestNameConversionsMatchDang` guards the cases a general-purpose case library
  gets wrong (`HTTPServer` → `http_server`, not `httpserver`), and
  `TestDangCrateNameMatchesRust` replays the dang recipe so the dang side cannot
  drift unnoticed.
- **The helper's crate name derivation cannot be spelled as the dang one is.**
  `toRustCrateName` is three `replaceMatches` calls with `${1}_${2}`
  replacements; goish's `regexp.ReplaceAllString` treats its replacement as
  literal text, so `${1}` expansion is not available to the Rust side. The
  helper folds the three substitutions into one left-to-right scan instead —
  don't "restore" the regexes, and don't reach for a case library either
  (that's what the acronym cases above are about). The scan is checked against
  the dang recipe by replaying it through goish's regexp engine, group
  expansion and all, in `TestDangCrateNameMatchesRust`.
- **The helper renders a subset of Go's `text/template`, and it is strict.** An
  action is `{{ .Field }}` and nothing else; unknown fields and unsupported
  actions are errors rather than Go's silent `<no value>`. Templates that need
  more than a field reference need the renderer extended first.
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

  The one name that does not work is `Dagger` itself: it derives the crate name
  `dagger`, which is also the vendored SDK's package name, and cargo refuses the
  pair with `package collision in the lockfile`. This is known and accepted, not
  an oversight — it is the cost of the vendored crate being called `dagger`
  rather than `dagger-sdk`. Reserving it would need the asymmetry above undone.
- **`initModule` seeds `dagger/`, and that is not a convenience.** The engine
  discovers a workspace's generators by *loading* every module in it, and loading
  a Rust module builds it. If a scaffolded module's `Cargo.toml` named a
  `dagger` that did not exist yet, it could not build, so it could not load,
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
  never again. `sdk/src/querybuilder.rs` is at the crate root for exactly the
  same reason: the generated bindings are written *against* it, so it cannot
  live in what generation replaces.
- **`sdk/tests/` is not vendored, and its `[[test]]` target is.**
  `vendoredSdkFiles` in `mod.dang` ships `Cargo.toml` and `src/**/*.rs` and
  nothing else, so a module gets a manifest naming a test target whose file it
  does not have. That is fine — cargo only resolves a target's path when it
  builds that target, and a module never builds the SDK's tests — but it is
  fine by luck rather than by design, so it is checked: a consumer crate
  path-depending on a `tests/`-less copy builds clean. Don't add a `[[bin]]` or
  `[[example]]` to `sdk/Cargo.toml` on the same assumption without re-checking.
- **An object type in a signature is matched by shape, not by lookup.** The
  macros have no schema to check a name against — the schema belongs to the
  engine the module will run against — so anything spelled as a plain type name
  is registered as an engine object of that name, and the check that it *is* one
  is that it implements `dagger::ObjectId`, which `sdk/codegen` emits for every
  object the schema lets it rebuild from an id. Two consequences. The name has to be read off the
  last path segment, so `render` in `sdk/macros/src/parse.rs` has to collapse
  ` : : ` — a path arrives as two `:` tokens, so `gen::Directory` renders as
  `gen : : Directory`, whose last segment is the whole string, and the engine is
  then told the argument is an object called that. And an object type in a
  signature only works *after* `dagger generate`: `src/gen` is a placeholder
  until then, which is why the templates and the two wrappers in
  `sdk/src/objects.rs` stay on scalars and IDs.
- **`serve()` may not use `src/gen`, only `querybuilder`.** Registration is
  ordinary API traffic — `typeDef`, `function`, `module` — so `sdk/src/module.rs`
  builds it with `Chain`/`Args`/`Leaf` and sends it with `engine::fetch`, naming
  the handful of fields it needs as string literals. Switching it to
  `dag().type_def()…` is the obvious-looking improvement and it deadlocks the
  bootstrap: `src/gen` is a placeholder until a module's first
  `dagger generate`, and the engine has to *load* the module — which runs
  registration — before generation can enumerate anything. This is the same
  cycle `initModule` seeds `dagger/` to break, seen from the other side, and
  it is why `templates/*/src/main.rs.tmpl` takes a `string` rather than a
  `Directory`. `ArgValueFields` in that file is the one `Fields` namespace
  written by hand for the same reason.
- **Generated code may only name what lives outside `src/gen`.** The bindings
  are written against `querybuilder.rs` and `engine.rs`, so anything they need
  — `ToArg`, `arg_string`, `Args`, `arg_list` — belongs there and not in the
  generated file. This is the same rule that keeps `Changeset`, `Workspace` and
  the query builder at the crate root, applied from the other side.
- **`fmt::Sprintf!` does not check its arguments.** It is a `macro_rules!` that
  hands the format string to `sprintf_impl` at run time, so a `%s` without an
  argument is not a compile error — Go's formatter writes `%!s(MISSING)` into
  the output and carries on. In `sdk/codegen` that artifact lands in a generated
  `mod.rs` and fails to compile in somebody's module instead. Keep format
  strings short, and never pass schema text as one; `TestNoFormatArtifacts`
  catches what slips through.
- **Two names in the emitted code cannot collide with the schema's.** The local
  holding a field's argument list is `__args`, and the one in the list-of-object
  loader is `__id`, because `snake_case` never produces a leading underscore and
  so no schema name can reach them. `withExec` really does have an argument
  named `args`, and the obvious spelling silently passed the half-built argument
  list to `arg_list` instead of the caller's command.
- **The engine owns module config.** `initModule` must not write `dagger.json`
  or `dagger-module.toml`; it returns only SDK-owned files. The engine merges in
  its own bookkeeping.

## Layout

| Path | What it is |
| --- | --- |
| `rust-sdk.dang` | SDK contract: `initModule`, `targetRuntime`, `modules`, `mod`, `@generate` |
| `mod.dang` | A managed module: vendors the SDK + generated bindings into `dagger/` |
| `template.dang` | Init template value type |
| `runtime/` | Build-only module runtime new Rust modules point at |
| `sdk/` | The `dagger` crate and `sdk/codegen`, its bindings generator |
| `templates/` | Starters for `dagger module init rust` |
| `helpers/render-template/` | Helper that renders a template for a module name |
| `.dagger/modules/tests/` | End-to-end checks for the Rust surface, in Dang |

## Working on this repo

Dang sources: `rust-sdk.dang`, `mod.dang`, `template.dang`, `runtime/main.dang`,
and `.dagger/modules/tests/*.dang`.
Format with `dang fmt -w`, but check the binary first — a `dang` older than the
`pub` keyword will silently strip `pub` from every declaration.

Verify the init helper directly:

```sh
cd helpers/render-template && cargo test
```

`cargo test` works, but not the way it usually does, and the arrangement is
easy to break. libtest is `std`, and its `panic_impl` collides with goish's, so
there are no `#[test]` functions: `tests/render_template.rs` is a
`harness = false` target whose `main` hands a list of functions to goish's
`testing::Main` — the shape `go test` generates — and cargo reads its exit
status. Three things hold it up. Every other target sets `test = false`, or
cargo compiles the lib and the bin a second time as test targets and hits the
lang-item collision anyway. `-C panic=abort` is in `.cargo/config.toml` rather
than only in the profiles, because cargo ignores `panic` for the test profile
and goish cannot unwind. And a new test has to be added to the list in `main`,
or it silently never runs.

The SDK crate's own suite — the query builder in `sdk/src/querybuilder.rs` —
and the bindings generator's suite run the same way, under the same rules:

```sh
cd sdk && cargo test
cd sdk/codegen && cargo test
```

`sdk/macros` is the one suite that does *not* work that way, and it is the
exception the rule allows: a proc-macro crate is built for the host and has no
goish in it, so its tests are ordinary `#[test]` functions in `src/tests.rs`.

```sh
cd sdk/macros && cargo test
```

They reach only the half of the crate that speaks in `String` — the type
mapping, and the text emitted for one signature. Anything that touches a
`TokenTree` is untestable there: the `proc_macro` API panics with "procedural
macro API is used outside of a procedural macro" when called from a test
binary, so the `Function` values are built by hand rather than parsed. The
parsing is covered by compiling a module against the crate, which is what the
`generate against a live engine` step below does.

`sdk/codegen`'s suite renders `tests/fixture.json`, a miniature schema, and
asserts on the *text* it emits. That is deliberate: the generator's contract is
the source it writes, and its failures — a local shadowing a parameter, a doc
comment with no item under it — surface as a compile error inside somebody's
module rather than anywhere near the generator. What the fixture cannot prove is
that the whole emitted surface compiles, because that needs the engine's real
1.2 MB schema. **For any change to what codegen emits, generate against a live
engine and build the crate with the result in place of `sdk/src/gen/mod.rs`**
(the recipe is in `sdk/README.md`), then put the placeholder back. Both bugs
found while writing the generator were ones only that step catches.

Run the shared SDK contract suite:

```sh
dagger -m github.com/dagger/sdk-sdk -W . check
```

That suite checks every SDK identically, so it stops at the contract. The
end-to-end checks for what is Rust-specific live in `.dagger/modules/tests`, a
Dang module discovered by a plain `dagger check`, and are the other half of the
manual step above: they scaffold a module from this working tree, generate it
against a live engine, drop a fixture over its `src/main.rs`, and call it.

```sh
dagger check tests
dagger check tests:client:decodes-object-lists
```

Building that fixture is what type-checks the whole generated surface, so the
`sdk/README.md` recipe is now a debugging aid rather than a step to remember —
but only when these checks actually run.

Three things about that module are load-bearing; its README argues each at
length:

- **The fixture is injected after the first `generate`, never before.** It names
  `dagger::gen` and calls `dag()`, and `src/gen` is a placeholder until then.
  Injecting earlier recreates the bootstrap cycle `initModule` exists to break.
- **The fixture ships `src/main.rs` and test data, and no `Cargo.toml`.** It is
  merged over a scaffolded module, so the goish pin, the link flags and the
  derived crate name come from `templates/default` at test time and cannot go
  stale. Its `pub struct FeatureMatrix` has to match the module name
  `feature-matrix`, though, because `Cargo.toml` was rendered from it.
- **One fixture, many functions.** Each fixture is a full cargo build of goish
  and ~20,000 generated lines with `lto = true`. Add a function, not a fixture.

Two things that will bite when editing that module. A `pub` function may not
return a type belonging to a dependency — the engine rejects the module with
"cannot return external type from dependency module" — which is why the
`mod-test` target is reached through a `let`. And a scaffolded module points at
the *published* runtime, because that is what `targetRuntime` returns, so the
suite rewrites its `[runtime] source` to the vendored copy; without that,
`runtime/main.dang` in the working tree is never exercised and a broken
`toRustCrateName` goes green.

To test a Rust change without an engine, build the crate against a local goish
checkout — swap the git dep for a path dep in a scratch copy rather than editing
the committed manifest.
