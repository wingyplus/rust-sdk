# rust-sdk

A Dagger module for managing Dagger modules that use the Rust SDK, plus the Rust
client library itself.

Ported from [`github.com/dagger/go-sdk`](https://github.com/dagger/go-sdk) — the
contract surface, module discovery and path handling are that module's, adapted
where Rust differs. The client library is built on
[goish](https://github.com/cogentica-ai/goish), a `no_std` reimplementation of
Go's standard library and runtime in Rust, so the Go SDK's client can be ported
rather than reinvented: goish supplies the `net/http`, `crypto/tls` and
`encoding/json` the engine session protocol needs, with no libc, no dynamic
loader and no garbage collector. A module links to one static binary.

Backed by [`github.com/dagger/polyfill`](https://github.com/dagger/polyfill).

> [!NOTE]
> **Working, with one gap.** The whole lifecycle runs: `dagger sdk install`,
> `dagger module init rust`, `dagger generate`, and `dagger call` against a
> module's functions, which execute as a single static binary. The gap is the
> other direction — a module cannot yet call the engine API, because the
> generated bindings are still a placeholder, so functions are limited to
> scalars. See [Status](#status).

## What's in here

| Path | What it is |
| --- | --- |
| `rust-sdk.dang`, `mod.dang`, `template.dang` | The SDK contract module — `initModule`, `targetRuntime`, and the `@generate` hook |
| `runtime/` | The module runtime new Rust modules reference. Build-only; see [its README](./runtime/README.md) |
| `sdk/` | The Rust client library (`dagger`) and its bindings generator |
| `templates/` | Starter templates for `dagger module init rust` |
| `helpers/render-template/` | Helper that renders a template for a given module name |

## Install

From your workspace root:

```sh
dagger sdk install github.com/wingyplus/rust-sdk
```

After install, the module is available in `dagger call` as `rust-sdk`.

Calls that return a `Changeset` will print the diff and prompt you to confirm
before writing anything to your workspace.

## Create a new module

```sh
dagger module init rust my-module
dagger generate
```

`initModule` seeds the SDK-owned template files, including a working
`dagger/`; the engine writes the module config and workspace entries. The
module builds and loads straight away — `generate` then replaces its bindings
with ones derived from your engine's schema.

Pick a starter with `--template`:

```sh
dagger module init rust my-module --template empty
```

`default` (the default) gives you a module with two example functions; `empty`
gives you a bare one.

You can also call the function directly for testing. `path` is required (the
engine supplies it in the dispatched path):

```sh
dagger call rust-sdk init-module --name my-module --path .dagger/modules/my-module
```

## Writing functions

A module's API is declared with two attributes. `#[dagger::object]` on the
`impl` block reads the signatures at compile time; `#[dagger::function]`
marks which methods are exposed. Anything unmarked stays private, so helpers
need no special treatment.

```rust
use goish::{fmt, int, string};

pub struct Build;

#[dagger::object]
impl Build {
    /// Build an image and return its tag.        // becomes the description
    #[dagger::function]
    pub fn image(
        &self,
        #[dagger(default = "alpine:3.21")] base: string,
        #[dagger(doc = "Tag to apply")] tag: Option<string>,
        #[dagger(default = 1)] jobs: int,
    ) -> string {
        fmt::Sprintf!("%s:%s", base, tag.unwrap_or(string("latest")))
    }

    // No attribute: invisible to `dagger call`.
    fn toolchain(&self) -> int { 0 }
}

#[goish::main]
fn main() {
    dagger::serve::<Build>()
}
```

```console
$ dagger call image --base ubuntu:24.04
ubuntu:24.04:latest
```

Method names are camelCased for the API, so `container_echo` is called as
`container-echo`. The method's `///` doc comment becomes the function's
description.

### Function options

What a function *is* goes in the marker attribute — the one slot Go writes its
`+` pragmas into.

| Option | Effect | Go SDK equivalent |
| --- | --- | --- |
| `generate` | The function is a generator; `dagger generate` runs it | `+generate` |

### Generators

`dagger generate` runs every generator it can find and applies what they return.
A Rust module declares one with the `generate` option:

```rust
use dagger::{Changeset, ObjectId, Session, Workspace};
use goish::string;

/// Write a generated file into the workspace.
#[dagger::function(generate)]
pub fn generate(&self, ws: Workspace) -> Changeset {
    let session = match Session::from_env() {
        Some(s) => s,
        None => dagger::fail(string("no engine session")),
    };

    // The workspace's current files: the "before" of the changeset.
    let before = match session
        .query(&(string("{loadWorkspaceFromID(id:")
            + dagger::json_string(&ws.id())
            + "){directory(path:\"/\"){id}}}"))
        .and_then(|d| dagger::field_string(&d, &["loadWorkspaceFromID", "directory", "id"]))
    {
        Ok(id) => id,
        Err(message) => dagger::fail(message),
    };

    // Write a file into it, and diff the result back against the before.
    let quoted = dagger::json_string(&before);
    let changes = match session
        .query(&(string("{loadDirectoryFromID(id:")
            + quoted.clone()
            + "){withNewFile(path:\"generated.txt\",contents:\"hello\"){changes(from:"
            + quoted
            + "){id}}}}"))
        .and_then(|d| {
            dagger::field_string(&d, &["loadDirectoryFromID", "withNewFile", "changes", "id"])
        }) {
        Ok(id) => id,
        Err(message) => dagger::fail(message),
    };

    Changeset::from_id(changes)
}
```

The engine calls a generator with nothing, so it holds it to a shape, and the
macro enforces the same rules at compile time rather than letting the module
fail to load:

- it returns `Changeset` — the changes to apply;
- every argument is one the engine can leave out: an `Option<T>`, one with a
  `default`, or a `Workspace`, which the engine injects itself.

That query-writing is the part the generated bindings will replace. Until they
land there is no `dag()`, so a generator reaches the engine through
[`Session::query`](./sdk/src/lib.rs), and `Changeset` and `Workspace` are ID
wrappers rather than real objects — enough to satisfy the contract, no more.

### Argument options

Options go in `#[dagger(...)]` on the parameter itself. The attribute is
stripped before `rustc` sees the function, so it needs nothing in scope.

| Option | Effect | Go SDK equivalent |
| --- | --- | --- |
| `default = <literal>` | Value used when the caller omits the argument | `+default` |
| `doc = "..."` | The argument's description | a doc comment |
| `deprecated = "..."` | Marks the argument deprecated | `+deprecated` |
| `default_path = "..."` | Load a `Directory`/`File` from the context directory | `+defaultPath` |
| `ignore = ["...", ...]` | Patterns to skip when loading a contextual argument | `+ignore` |

`doc` is the one option that cannot mirror Go: Rust has no doc comments on
parameters, so a description has to be written as an option.

An argument is optional when its type is `Option<T>` **or** when it has a
`default` — there is no `+optional` marker to write.

`default_path` and `ignore` are parsed and forwarded, but the engine accepts
them only on object types (`Directory`, `File`). Those need the generated
bindings, so using either today is a compile error naming the parameter, rather
than a failure at module load.

### Supported types

`string` (also `String` and `&str`), `int`, `bool`, and `Option<T>` of each. A
function returning nothing maps to `VOID_KIND`.

`Changeset` and `Workspace` are understood too, since the generator contract is
written in terms of them; they cross the boundary as engine IDs. Every other
object type, `Container` among them, is rejected at compile time until the
bindings are generated.

### Checks

A check validates the project — a test, a lint, a scan — and passes or fails.
`dagger check` discovers and runs every check a module exposes. Mark one with
`#[dagger::check]`, the Rust spelling of the Go SDK's `// +check` pragma and
the TypeScript SDK's `@check()` decorator:

```rust
#[dagger::object]
impl Build {
    /// The sources are formatted.
    #[dagger::check]
    pub fn fmt(&self) {
        if !self.sources_are_formatted() {   // a private helper, as above
            dagger::fail(string("sources are not formatted"))
        }
    }
}
```

```console
$ dagger check
```

`check` implies `function`, so a check is also callable as `dagger call fmt`
and the two attributes need not both be written. A check fails the way any
function fails — by exiting non-zero, which `dagger::fail` does with a
message on stderr.

A check takes no caller arguments: `dagger check` runs it with none, so every
argument must be an `Option<T>` or carry a `default`. The engine's response to a
check it cannot run is to leave it out of the check tree, where it would simply
never appear, so the macro rejects a required argument at compile time instead.

## Generate SDK files

For a single module:

```sh
dagger call rust-sdk mod --path my-module generate
```

For every Rust SDK module visible from your current directory:

```sh
dagger generate
```

Generation vendors the `dagger` crate, together with the API bindings
generated from your engine's schema, into `<module>/dagger/`. **Commit it** —
the runtime builds from the committed sources and never regenerates them.

To exclude a directory tree from bulk generation, drop an empty
`.dagger-rust-sdk-skip-generate` file at or above the module root:

```sh
touch some/fixture/.dagger-rust-sdk-skip-generate
```

The same `dagger generate` also runs whatever generators your own modules
declare — see [Generators](#generators). Refreshing `dagger/` is this SDK's
generator; yours run alongside it.

## How a Rust module is built

Rust has no built-in engine runtime, so new modules point at this repository's
[`runtime/`](./runtime) module instead of a bare runtime name:

```toml
[runtime]
  source = "github.com/wingyplus/rust-sdk/runtime"
```

`mod` still resolves modules that declare the bare name `rust`, so a module
written against a future built-in runtime keeps working.

Two invariants hold the pieces together, and both are load-bearing:

- **goish is pinned by git rev**, in `sdk/Cargo.toml`, `sdk/codegen/Cargo.toml`,
  `helpers/render-template/Cargo.toml` and every `templates/*/Cargo.toml.tmpl`.
  It is not on crates.io. The pins that meet in one build — the SDK crate's and
  the module's — must name the same rev, since cargo treats two revs as two
  different crates and linking a module against two copies of the goish runtime
  fails on duplicate symbols. Bump all of them together.
- **The binary's name is derived, not configured.** `rust_crate_name` in
  `helpers/render-template/src/lib.rs` writes the cargo `[package]` name at init
  time; `toRustCrateName` in `runtime/main.dang` recomputes it at call time to
  find the binary. They must stay byte-for-byte identical — a divergence builds
  fine and then fails to start. `TestNameConversionsMatchDang` guards the cases
  a general-purpose case library gets wrong, and `TestDangCrateNameMatchesRust`
  replays the dang recipe against the helper so neither side can drift alone.

Each module also carries a `.cargo/config.toml`. It is not boilerplate: it names
the target tuple explicitly (so goish's bare-metal link flags stay off
host-built proc-macro crates) and passes `-nostartfiles -nodefaultlibs -static`,
without which a `no_std` goish binary does not link. Cross-compiling on a
non-x86_64 engine needs one more thing, a linker that understands `-m64`, and
the runtime supplies it through the environment rather than this file — so the
committed `.cargo/config.toml` is identical on every architecture.

## Manage dependencies and the engine version

Editing a module's dependencies or its required engine version is identical
across SDKs, so the core CLI owns it:

```sh
dagger module deps add github.com/some/module
dagger module engine require-latest
```

## Status

What works today:

- The full SDK contract surface: `initModule`, `targetRuntime`, `modules`,
  `mod`, and the `@generate` hook, with cwd-anchored discovery ported from the
  Go SDK.
- `runtime/`, which builds a module from its committed sources and sets the
  resulting static binary as the entrypoint.
- Both starter templates, which render and build to a stripped, statically
  linked ELF with no interpreter.
- Any engine platform. goish targets Linux x86_64 only, so a module binary is
  always x86_64 and is served from a `linux/amd64` container. The build itself
  runs on the engine's own platform and cross-links, so on an arm64 engine
  (Apple Silicon) compilation is native — only the finished binary is emulated.

What is stubbed:

- **The client's typed API.** A module can be *called*, but it cannot yet *call*
  the engine back with types: there is no `dag()`, because that needs the
  generated bindings below, so a module that must reach the engine writes the
  query itself against `Session::query`. Function signatures are limited to
  `string`, `int`, `bool`, and the `Changeset`/`Workspace` ID wrappers a
  generator is declared with.
- **`sdk/codegen`.** It reads and validates the introspection schema and emits a
  placeholder module. The real output should mirror the Go SDK's
  `dagger.gen.go`: one type per GraphQL object, each method appending to a
  lazily-built selection sent only when a leaf value is requested. Parsing the
  schema is goish's `encoding/json`; no serde equivalent is needed.

Function declaration and dispatch **work**: `#[dagger::object]`,
`#[dagger::function]` and `#[dagger::check]` read signatures at compile time and
emit a static table the entrypoint walks, with argument options in
`#[dagger(...)]` and function options — `generate` — in the marker itself. This
is the one piece with no Go analogue: the Go SDK recovers signatures by parsing
the user's package, so it is proc-macros rather than a port.

`initClient` (typed client generation for non-module consumers) is an optional
part of the SDK contract and is not implemented.

## Check this repository

Run the shared SDK contract suite against it:

```sh
dagger -m github.com/dagger/sdk-sdk -W . check
```

Test the template helper directly:

```sh
cd helpers/render-template && cargo test
```

The helper is `no_std` on goish like the rest of the repository, so the suite is
a `harness = false` target built on goish's `testing` package rather than
`#[test]` functions — libtest is `std`, and its panic handler collides with
goish's.

## Licence

Apache 2.0. Note that a built module also carries goish's licences: goish's own
code is MIT, and the parts of it ported from Go's standard library remain
BSD-3-Clause (© The Go Authors). Both must accompany redistribution of a module
binary.
