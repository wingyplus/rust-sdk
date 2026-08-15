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
| `sdk/` | The Rust client library (`dagger-sdk`) and its bindings generator |
| `templates/` | Starter templates for `dagger module init rust` |
| `helpers/render-template/` | Go helper that renders a template for a given module name |

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
`dagger_sdk/`; the engine writes the module config and workspace entries. The
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

A module's API is declared with two attributes. `#[dagger_sdk::object]` on the
`impl` block reads the signatures at compile time; `#[dagger_sdk::function]`
marks which methods are exposed. Anything unmarked stays private, so helpers
need no special treatment.

```rust
use goish::{fmt, int, string};

pub struct Build;

#[dagger_sdk::object]
impl Build {
    /// Build an image and return its tag.        // becomes the description
    #[dagger_sdk::function]
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
    dagger_sdk::serve::<Build>()
}
```

```console
$ dagger call image --base ubuntu:24.04
ubuntu:24.04:latest
```

Method names are camelCased for the API, so `container_echo` is called as
`container-echo`. The method's `///` doc comment becomes the function's
description.

### Argument options

Options go in `#[dagger(...)]` on the parameter itself. The attribute is
stripped before `rustc` sees the function, so it needs nothing in scope;
`#[dagger_sdk(...)]` is accepted too.

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
function returning nothing maps to `VOID_KIND`. Object types such as `Container`
are rejected at compile time until the bindings are generated.

## Generate SDK files

For a single module:

```sh
dagger call rust-sdk mod --path my-module generate
```

For every Rust SDK module visible from your current directory:

```sh
dagger generate
```

Generation vendors the `dagger-sdk` crate, together with the API bindings
generated from your engine's schema, into `<module>/dagger_sdk/`. **Commit it** —
the runtime builds from the committed sources and never regenerates them.

To exclude a directory tree from bulk generation, drop an empty
`.dagger-rust-sdk-skip-generate` file at or above the module root:

```sh
touch some/fixture/.dagger-rust-sdk-skip-generate
```

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

- **goish is pinned by git rev**, in `sdk/Cargo.toml`, `sdk/codegen/Cargo.toml`
  and every `templates/*/Cargo.toml.tmpl`. It is not on crates.io. The pins that
  meet in one build — the SDK crate's and the module's — must name the same rev,
  since cargo treats two revs as two different crates and linking a module
  against two copies of the goish runtime fails on duplicate symbols. Bump all
  of them together.
- **The binary's name is derived, not configured.** `rustCrateName` in
  `helpers/render-template/main.go` writes the cargo `[package]` name at init
  time; `toRustCrateName` in `runtime/main.dang` recomputes it at call time to
  find the binary. They must stay byte-for-byte identical — a divergence builds
  fine and then fails to start. `TestNameConversionsMatchDang` guards the cases
  a general-purpose case library gets wrong.

Each module also carries a `.cargo/config.toml`. It is not boilerplate: it names
the target tuple explicitly (so goish's bare-metal link flags stay off
host-built proc-macro crates) and passes `-nostartfiles -nodefaultlibs -static`,
without which a `no_std` goish binary does not link.

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

What is stubbed:

- **The client's typed API.** A module can be *called*, but it cannot yet *call*
  the engine back: there is no `dag()`, because that needs the generated
  bindings below. Functions are limited to `string`, `int` and `bool`.
- **`sdk/codegen`.** It reads and validates the introspection schema and emits a
  placeholder module. The real output should mirror the Go SDK's
  `dagger.gen.go`: one type per GraphQL object, each method appending to a
  lazily-built selection sent only when a leaf value is requested. Parsing the
  schema is goish's `encoding/json`; no serde equivalent is needed.

Function declaration and dispatch **work**: `#[dagger::object]` and
`#[dagger::function]` read signatures at compile time and emit a static table
the entrypoint walks, with argument options in `#[dagger(...)]`. This is the one
piece with no Go analogue — the Go SDK recovers signatures by parsing the user's
package — so it is proc-macros rather than a port.

`initClient` (typed client generation for non-module consumers) is an optional
part of the SDK contract and is not implemented.

## Check this repository

Run the shared SDK contract suite against it:

```sh
dagger -m github.com/dagger/sdk-sdk -W . check
```

Test the template helper directly:

```sh
cd helpers/render-template && go test ./...
```

## Licence

Apache 2.0. Note that a built module also carries goish's licences: goish's own
code is MIT, and the parts of it ported from Go's standard library remain
BSD-3-Clause (© The Go Authors). Both must accompany redistribution of a module
binary.
