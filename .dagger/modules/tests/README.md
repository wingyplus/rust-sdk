# tests

End-to-end checks for the Rust half of this SDK, written in Dang.

```sh
dagger check                       # everything, including sdk-sdk's contract suite
dagger check tests             # just these
dagger check tests:client:decodes-object-lists
```

## What this covers, and what it does not

`dagger -m github.com/dagger/sdk-sdk -W . check` already covers the SDK
*contract* — `sdk install`, `module init`, `generate`, the module verbs,
dependency chains, `initModule` — and covers it identically for every SDK. None
of that is repeated here.

What is here is what only a Rust module can show:

| group | covers | why nothing else reaches it |
| --- | --- | --- |
| `declaration` | `default`, `doc`, `deprecated`, `Option<T>`, Void returns, fallible returns in both error types, failure propagation and the message that comes with it | `sdk/macros` is a proc-macro crate; the `proc_macro` API panics outside a macro expansion, so its own tests build `Function` values by hand and never parse a signature |
| `objects` | `Directory` in, `Container` out, `default_path`, `ignore` | an object crosses the boundary as an engine ID, and nothing until now called `ObjectId::from_id`/`to_id` against a real engine |
| `client` | leaves, scalar lists, **object lists**, enums, opts structs, `fetch`, nested `select`, chaining | `sdk/codegen`'s suite asserts on emitted *text* against a miniature schema |
| `verbs` | `dagger check` and `dagger generate` discovering a Rust module | — |

Two things come free with the fixture merely building: the whole generated
surface type-checks against the engine's own schema — 31,000 lines against 1.0,
and the manual step `sdk/README.md` describes — and the goish pin, link flags and
derived crate name are all exercised as a real module would use them.

Known gap: input objects (`BuildArg` and friends). Reaching one naturally costs
a `dockerBuild`, which is minutes for one `ToArg` impl; `sdk/codegen`'s suite
pins the text emitted for them.

## How it works

`target.dang` stages a workspace, `mod-test` calls into it.

1. Vendor this repository into a scratch git workspace inside a runner
   container holding a pinned Dagger CLI.
2. `dagger sdk install` → `dagger module init` → repoint the scaffolded module
   at the *vendored* runtime → `dagger generate`.
3. Overwrite the module's `src/main.rs` with `fixtures/feature-matrix`.
4. `dagger generate` again, which is when the fixture's own generator registers.
5. Hand the resulting `Directory` to
   [`mod-test`](https://github.com/dagger/sdk-sdk/tree/main/mod-test), which owns
   the calling and the assertion vocabulary.

Every command runs under `experimentalPrivilegedNesting`, so the nested CLI
shares this engine — and with it the `rust-sdk-cargo-*` and
`rust-sdk-module-target-*` cache volumes a normal build uses.

Three things about this arrangement are deliberate and easy to break:

- **The fixture is injected *after* the first generate.** `dagger::gen` is a
  19-line placeholder with no `dag()` until then, so a fixture naming
  `gen::Directory` cannot compile before it. Moving the injection earlier
  recreates the bootstrap cycle `initModule` seeds `dagger/` to break.
- **The fixture has no `Cargo.toml`.** It is merged over a scaffolded module, so
  the goish rev, the bare-metal link flags and the derived crate name all come
  from `templates/default` at test time. A fixture carrying its own manifest
  would be one more place to bump the goish pin, and would go stale silently.
- **There is one fixture, with many functions.** Each fixture is a full cargo
  build of goish plus the whole generated surface, with `lto = true` and
  `codegen-units = 1`. Add a function, not a fixture.

`localRuntime` in `target.dang` is the reason for step 2's third command:
`targetRuntime` returns a *published* ref, so without the rewrite a scaffolded
module would be built by the runtime on GitHub while the SDK crate under test
came from this checkout — and the suite would go green with a broken
`toRustCrateName` in `runtime/main.dang`.

## Cost

A cold run compiles goish, the vendored SDK and the fixture, and is measured in
tens of minutes; a warm one is dominated by the nested `dagger call` sessions.
Every check derives from one `preparedWorkspace`, so the lifecycle is
content-addressed and evaluated once for the whole suite rather than once per
check.

While iterating, drive the lifecycle directly instead of through a check:

```sh
dagger call -m .dagger/modules/tests target run --args '["version"]' stdout
dagger call -m .dagger/modules/tests target prepared-workspace entries
dagger call -m .dagger/modules/tests target module-file --path dagger/src/gen/mod.rs
```

And iterate on `fixtures/feature-matrix/src/main.rs` outside this suite
entirely: scaffold a throwaway module, `dagger generate`, copy the fixture in,
and use a normal `cargo build` loop.

## Things that already caught someone out

- **A green check is not a built module.** `dagger generate` reports a module it
  cannot load and carries on with the generators it did find, so a fixture that
  fails to compile leaves every file-reading check passing.
  `verbs:module-builds-and-serves` exists for that reason: introspecting the
  module cannot succeed unless cargo did. Keep it, and treat it as the first
  thing to read when the suite goes strange.
- **`dagger check 'tests:*'` matches nothing.** Patterns are split on `:` and
  compared segment by segment, so a two-segment pattern never matches a
  three-segment check. Use `tests`, `tests:*:*` or `tests/**`.
- **A boolean flag takes its value attached.** `--flag=false`; the split form
  makes the CLI read `false` as a subcommand.
- **GraphQL hides deprecated things from introspection.** Asking a module's type
  for a deprecated argument without `args(includeDeprecated: true)` returns the
  function with *no arguments at all*, which reads exactly like a macro that
  dropped it.
- **A `pub` function may not return a dependency's type.** The engine refuses
  the module with "cannot return external type from dependency module", so the
  `mod-test` target is reached through a `let`.
- **`dang fmt -w` may strip every `pub`.** CLAUDE.md records this; check the
  binary before formatting these files.

## Caveats

- The suite is flaky under emulation, the same way `CLAUDE.md` records for the
  sdk-sdk suite. Repeat a single check rather than trusting one whole-suite run.
- `mod-test` pins its own CLI release, and this module reads that same version
  rather than declaring a second one — two different releases would mean one
  side producing a workspace the other cannot load.
