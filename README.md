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
> **Working.** The whole lifecycle runs: `dagger sdk install`,
> `dagger module init rust`, `dagger generate`, and `dagger call` against a
> module's functions, which execute as a single static binary. The bindings for
> the other direction are generated too, and reach a live engine; a function's
> own signature may name engine objects as well as scalars. What is left is
> narrower — lists in a signature, and handing an object straight to a client
> method rather than as its id. See [Status](#status).

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
use dagger::gen;
use dagger::ObjectId;

/// Write a generated file into the workspace.
#[dagger::function(generate)]
pub fn generate(&self, ws: gen::Workspace) -> Result<gen::Changeset, string> {
    // The workspace's current files: the "before" of the changeset.
    let before = ws.directory("/");
    let before_id = before.to_id()?;

    // Write a file into it, and diff the result back against the before.
    Ok(before
        .with_new_file("generated.txt", "hello")
        .changes(before_id))
}
```

The engine calls a generator with nothing, so it holds it to a shape, and the
macro enforces the same rules at compile time rather than letting the module
fail to load:

- it returns `Changeset` — the changes to apply, directly or as
  `Result<Changeset, string>`;
- every argument is one the engine can leave out: an `Option<T>`, one with a
  `default`, or a `Workspace`, which the engine injects itself.

The example names `dagger::gen`, so it only compiles *after* the module's first
`dagger generate`. Before then those bindings are a placeholder, and the
`Changeset` and `Workspace` at the crate root are what a module has: hand-written
ID wrappers, enough to satisfy the same signature while a scaffolded module
builds for the first time. That is why the starter templates stay on scalars.

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

`default_path` and `ignore` apply to a `Directory` or `File` argument only, the
way the engine has it: writing either on anything else is a compile error naming
the parameter, rather than a module that fails to load. Such an argument is not
declared optional — the function always receives one — but the caller may still
leave it out, since the engine loads it from the context directory. That is
enough to satisfy the "no required arguments" rule a check or a generator is
held to.

### Supported types

`string` (also `String`), `int`, `bool`, and `Option<T>` of each. A function
returning nothing maps to `VOID_KIND`.

Object types too: anything named as a plain type — `Directory`, `Container`,
`Changeset`, `Workspace` — is declared to the engine as that object, under the
last segment of the path, so `gen::Directory` and `Directory` are the same
declaration. It has to be a type that implements `dagger::ObjectId`, which the
generated bindings do for every object the engine has a loader for, so a
misspelling is a compile error about that trait.

Lists are not supported in either direction, and neither is an optional return.
Both are compile errors naming what is unsupported.

### Failing

A function returns its value directly, or as `Result<T, string>`:

```rust
/// What the container printed.
#[dagger::function]
pub fn out(&self) -> Result<string, string> {
    dag().container().from("alpine:3.21").with_exec(&["echo", "hi"]).stdout()
}
```

Every client method is fallible — reaching the engine is a round trip — so a
`Result` return is what lets `?` carry a failure out of the function instead of
`unwrap_or_else(|m| dagger::fail(m))` at each call. An `Err` ends the call
exactly as [`dagger::fail`](#checks) does: the message on stderr, a non-zero
exit.

The engine is told what the function *produces*, so `Result<T, string>` and `T`
declare the same thing — a caller sees no trace of the difference.

The error is goish's `string` or its `error`, whichever the work at hand fails
with. The client fails with the message itself; goish's own APIs fail with an
`error`, so a function doing that kind of work says so:

```rust
/// The first line of a file the module carries.
#[dagger::function]
pub fn first_line(&self, path: string) -> Result<string, errors::error> {
    let (data, err) = os::ReadFile(path);
    if err != nil {
        return Err(err);
    }
    Ok(strings::SplitN(string(data), "\n", 2)[0].clone())
}
```

The two cross with `map_err(errors::New)` one way and `dagger::error_message`
the other — which is what the dispatch calls, rather than `Error()`, because
that method panics on the nil error. Any other error type is a compile error
naming it: goish has no `Display` to read a message off one with.

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
- `sdk/codegen`, which turns an engine's introspection schema into the whole
  typed client — around 20,000 lines from the v0.21 schema. One type per GraphQL
  object, holding the transport it was reached through and a lazily-built
  selection that is only sent when a leaf value is asked for; enums, input
  objects and per-field option structs alongside. It parses the schema with
  goish's `encoding/json`; no serde equivalent is needed. `dag()` takes no
  argument: the engine puts the session in the module process's environment, so
  the client opens it itself (`dag_with(transport)` when you have your own).

  ```rust
  let ctr = dag().container().from("alpine").with_exec(&["echo", "hi"]);
  let out = ctr.stdout()?;                                  // one round trip
  let (platform, size) = ctr.fetch(|c| (                    // also one round trip
      c.platform(),
      c.file("/etc/os-release").select(|f| f.size()),
  ))?;
  ```

- Object types in a signature: a function can take a `Directory` and return a
  `Container`, alongside `string`, `int` and `bool`. An object crosses the
  boundary as an engine ID: an argument is rebuilt into a real client object
  before the function sees it — over the session `dag()` opens, so nothing is
  threaded through — and a returned one is resolved back to its ID, which is a
  round trip and so can fail. `codegen` emits the `ObjectId` impl that does both
  for every object the engine lets it rebuild from an id — a per-type
  `loadXFromID` on engines through v0.21, or the single `node(id:)` that Dagger
  1.0 replaced them with.

  ```rust
  /// Grep a directory for a pattern.
  #[dagger::function]
  pub fn grep_dir(&self, directory_arg: Directory, #[dagger(default = "hello")] pattern: string) -> Result<string, string> {
      let source = directory_arg.to_id()?;
      dag().container().from("alpine:latest")
          .with_mounted_directory("/mnt", source)
          .with_workdir("/mnt")
          .with_exec(&["grep", "-R", pattern.as_ref(), "."])
          .stdout()
  }
  ```

- Fallible functions. A function returns its value directly or as
  `Result<T, string>`, as above — or as `Result<T, error>`, goish's own error
  type. The engine is told what the function produces either way, and an `Err`
  reaches the caller as the message `dagger::fail` would have written. That is
  what makes `?` usable against a client whose every method is fallible.

What is stubbed:

- **Handing an object back to a client method.** As the example above shows, a
  `Directory` reaches `withMountedDirectory` as `to_id()` rather than as itself:
  the schema types that argument `DirectoryID`, which these bindings map to
  `string`. Passing the object would mean resolving its ID while the query is
  being built, and a method that extends the chain returns an object rather than
  a `Result` — so it is the query builder that would have to change, not the
  generator.

- **Lists, and optional returns.** A function's arguments and return are one
  value each: `slice<string>` in a signature, or `Option<T>` in return position,
  are compile errors naming what is unsupported rather than confusing failures
  further along.

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

That suite checks every SDK the same way, so it stops at the contract. The
end-to-end checks for the Rust half — the attribute macros and the generated
client, exercised by building a real module and calling it — live in
[`.dagger/modules/tests`](./.dagger/modules/tests) and are discovered by a plain
`dagger check`:

```sh
dagger check                        # both suites
dagger check tests              # just the Rust end-to-end checks
dagger check tests:client:decodes-object-lists
```

Building that suite's fixture also type-checks the whole generated client
against a live engine's schema, which is otherwise a manual step — see
[`sdk/README.md`](./sdk/README.md). Expect tens of minutes cold; the module's
README explains the cost and how to iterate without paying it.

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
