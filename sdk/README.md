# dagger-sdk

The Dagger client library for Rust modules, plus the generator that produces its
API bindings.

```text
.
├── Cargo.toml     the `dagger-sdk` crate — no_std, depends on goish
├── src/lib.rs     session parameters and the module entrypoint
├── src/module.rs  what a module declares, and how a call reaches it
├── src/objects.rs the engine objects a module can name; NOT under src/gen
├── src/gen/       API bindings; replaced wholesale by `dagger generate`
├── macros/        the attribute macros that declare objects and functions
└── codegen/       no_std binary: introspection schema in, `src/gen/` out
```

This crate is never consumed from crates.io or by git ref. `dagger generate`
vendors it into each module as `dagger_sdk/`, with `src/gen/` replaced by
bindings generated from that module's own schema, and the module depends on it
by path. Modules commit the result; the runtime builds from it and never
regenerates it.

`codegen/` is `no_std` and built on goish like everything else here, so it
carries its own `.cargo/config.toml` and its binary lands under the target tuple
rather than directly in `release/` — `mod.dang` accounts for that when it copies
the binary out. goish is its only dependency, so `dagger generate` needs no
crates.io access beyond the toolchain image, and the schema parsing the real
generator needs comes from goish's `encoding/json` rather than serde.

## Working on it standalone

```sh
cd sdk && cargo check
```

That is enough to type-check against goish. Note there are no `[profile]`
sections in `Cargo.toml`: this crate is always a non-root package in a real
build, where cargo ignores a dependency's profiles and warns about them. The
`panic = "abort"` that goish requires to link is set by the module's own
`Cargo.toml`.

## Status

See the [repository README](../README.md#status). The session protocol and
`serve()` are real — a module registers, dispatches, and can declare a generator
— but no generated binding is, so anything a module needs from the engine it
asks for with `Session::query`.
