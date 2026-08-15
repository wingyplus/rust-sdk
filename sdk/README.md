# dagger-sdk

The Dagger client library for Rust modules, plus the generator that produces its
API bindings.

```text
.
├── Cargo.toml     the `dagger-sdk` crate — no_std, depends on goish
├── src/lib.rs     session parameters and the module entrypoint
├── src/gen/       API bindings; replaced wholesale by `dagger generate`
└── codegen/       host binary: introspection schema in, `src/gen/` out
```

This crate is never consumed from crates.io or by git ref. `dagger generate`
vendors it into each module as `dagger_sdk/`, with `src/gen/` replaced by
bindings generated from that module's own schema, and the module depends on it
by path. Modules commit the result; the runtime builds from it and never
regenerates it.

`codegen/` is deliberately dependency-free and deliberately *not* `no_std` — it
is an ordinary hosted binary built for the build host, so it must not inherit
the module's bare-metal link flags. Keeping it free of crates.io dependencies
means `dagger generate` needs no registry access beyond the toolchain image.

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

Scaffold — see the [repository README](../README.md#status). `Session::from_env`
is real; `serve()` is not, and neither is any generated binding.
