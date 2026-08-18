//! The one fixture module the end-to-end suite calls into.
//!
//! Every function here exists to be asserted on from Dang; see the check groups
//! in `.dagger/modules/tests/checks-*.dang`. There is exactly one fixture, and it
//! is deliberately crowded: each fixture is a full cargo build of goish, the
//! vendored SDK and the ~20,000 generated lines under `dagger/src/gen`, with
//! `lto = true` and `codegen-units = 1` like any module this SDK scaffolds. One
//! more function costs nothing; one more fixture costs minutes.
//!
//! # What is not here
//!
//! No `Cargo.toml`, no `.cargo/config.toml`, no `dagger/`. This directory is
//! merged *over* a module the suite scaffolds with `dagger module init`, so the
//! goish git rev, the bare-metal link flags, the target tuple and the derived
//! crate name all come from `templates/default` at test time. A fixture that
//! carried its own manifest would be one more place the goish pin has to be
//! bumped, and it would go stale silently.
//!
//! The type is `FeatureMatrix` because the suite scaffolds the module as
//! `feature-matrix`: `rust_type_name` in `helpers/render-template/src/lib.rs`
//! derives `FeatureMatrix` from that, and `Cargo.toml` names the crate
//! `feature_matrix`. Renaming the module without renaming this struct compiles
//! to a missing-type error rather than anything informative.
//!
//! # Why this can name `dagger::gen`
//!
//! Because the suite injects it *after* the module's first `dagger generate`.
//! Before then `dagger::gen` is a 19-line placeholder with no `dag()` and no
//! object types, which is the bootstrap cycle `initModule` seeds `dagger/` to
//! break — see `target.dang`.
//!
//! # Known gap
//!
//! Input objects (`BuildArg` and friends) are not exercised: the natural way to
//! reach one is a `dockerBuild`, which costs an image build for one `ToArg`
//! impl. `sdk/codegen`'s own suite pins the text emitted for them.

#![no_std]
#![no_main]

use dagger::gen::{self, dag};
use dagger::ObjectId;
use goish::{errors, fmt, int, slice, string, strings};

/// A module exercising the whole Rust SDK surface.
pub struct FeatureMatrix;

/// Every Rust SDK feature the end-to-end suite asserts on.
///
/// This comment is the assertion: the macro reads the `///` on the annotated
/// `impl` block and registers it as the object's description *and* the
/// module's. It deliberately does not repeat the struct's doc above, so a
/// check that finds this text has found the block's comment rather than
/// whatever else happened to be lying around.
#[dagger::object]
impl FeatureMatrix {
    // ---------------------------------------------------------------- declaration

    /// Return the argument, unchanged.
    #[dagger::function]
    pub fn echo(&self, string_arg: string) -> string {
        string_arg
    }

    /// Return the value, which defaults to "hello".
    #[dagger::function]
    pub fn with_default(&self, #[dagger(default = "hello")] value: string) -> string {
        value
    }

    /// Return the number, which defaults to 3.
    #[dagger::function]
    pub fn with_default_int(&self, #[dagger(default = 3)] n: int) -> int {
        n
    }

    /// Return the flag, which defaults to true.
    #[dagger::function]
    pub fn with_default_bool(&self, #[dagger(default = true)] flag: bool) -> bool {
        flag
    }

    /// Return the factor, which defaults to 1.5.
    #[dagger::function]
    pub fn with_default_float(&self, #[dagger(default = 1.5)] factor: f64) -> f64 {
        factor
    }

    /// Return the number, unchanged.
    ///
    /// A float goes out through `encode_float` rather than through
    /// `encode_int`, and comes back in through the `float` accessor: the two
    /// are a pair, and a mismatch between them is a value that changes on the
    /// way through rather than a failure.
    #[dagger::function]
    pub fn float_round_trip(&self, factor: f64) -> f64 {
        factor
    }

    /// Halve the number.
    ///
    /// A float *return* that is not the argument echoed back, so the encoding
    /// is shown on a value the caller did not spell: `7 / 2` is `3.5`, which an
    /// integer encoder would render as `3`.
    #[dagger::function]
    pub fn half(&self, factor: f64) -> f64 {
        factor / 2.0
    }

    /// Report whether a value was supplied.
    #[dagger::function]
    pub fn optional_arg(&self, value: Option<string>) -> string {
        match value {
            Some(value) => string("some:") + value,
            None => string("none"),
        }
    }

    /// Return the value.
    #[dagger::function]
    pub fn documented(
        &self,
        #[dagger(doc = "The value to echo back")] value: string,
    ) -> string {
        value
    }

    /// Return the value, from an argument nobody should use any more.
    #[dagger::function]
    pub fn deprecated_arg(
        &self,
        #[dagger(deprecated = "use documented instead")] value: Option<string>,
    ) -> string {
        value.unwrap_or_else(|| string("unset"))
    }

    /// Return a fixed value, from a function nobody should call any more.
    ///
    /// The function-level twin of `deprecated_arg` above. The engine takes a
    /// reason on either, but the two are spelled differently because a method
    /// has no `#[dagger(...)]` of its own: a function's goes in the marker
    /// attribute, next to `generate`.
    #[dagger::function(deprecated = "use echo instead")]
    pub fn deprecated_function(&self) -> string {
        string("deprecated-function")
    }

    /// Fail on purpose.
    #[dagger::function]
    pub fn boom(&self) -> string {
        dagger::fail(string("intentional failure"))
    }

    /// Return what a container printed, fallibly.
    ///
    /// A `Result` return is what lets `?` carry a client failure out of the
    /// function: every generated method is fallible, since reaching the engine
    /// is a round trip, and the alternative is
    /// `unwrap_or_else(|m| dagger::fail(m))` at each call — which is what every
    /// other client function in this fixture writes.
    #[dagger::function]
    pub fn try_leaf(&self) -> Result<string, string> {
        dag()
            .container()
            .from("alpine:3.22")
            .with_exec(&["echo", "-n", "try-leaf"])
            .stdout()
    }

    /// Fail by returning `Err`.
    ///
    /// The other end of `boom` above: the same non-zero exit and the same
    /// message on stderr, reached by returning rather than by diverging.
    #[dagger::function]
    pub fn try_boom(&self) -> Result<string, string> {
        Err(string("intentional Err"))
    }

    /// Fail with a goish `error` rather than with a message.
    ///
    /// The other error a fallible function may carry. goish's own APIs hand
    /// back an `error`, so a function whose work is goish's rather than the
    /// client's fails that way too, and the dispatch reads the message off it.
    /// A client failure crosses into that shape with `map_err(errors::New)`,
    /// which is the line below.
    #[dagger::function]
    pub fn try_error(
        &self,
        #[dagger(default = false)] boom: bool,
    ) -> Result<string, errors::error> {
        let out = dag()
            .container()
            .from("alpine:3.22")
            .with_exec(&["echo", "-n", "try-error"])
            .stdout()
            .map_err(errors::New)?;

        if boom {
            return Err(errors::New("intentional goish error"));
        }
        Ok(out)
    }

    /// Return an object, fallibly.
    ///
    /// Two failures meet in one signature: the `?` on the id round trip the
    /// function itself makes, and the one the dispatch adds to resolve the
    /// Container it hands back.
    #[dagger::function]
    pub fn try_container_out(&self) -> Result<gen::Container, string> {
        let id = dag().directory().with_new_file("hello.txt", "try-object").to_id()?;

        Ok(dag()
            .container()
            .from("alpine:3.22")
            .with_mounted_directory("/mnt", id)
            .with_exec(&["cat", "/mnt/hello.txt"]))
    }

    /// A check that passes by returning `Ok`.
    ///
    /// The Void half of a fallible return, and the shape a real check wants:
    /// the work is fallible, and what it decides is the `Err`.
    #[dagger::check]
    pub fn try_check(&self) -> Result<(), string> {
        let out = dag()
            .container()
            .from("alpine:3.22")
            .with_exec(&["echo", "-n", "ok"])
            .stdout()?;

        if out != "ok" {
            return Err(string("expected ok, got ") + out);
        }
        Ok(())
    }

    /// Return nothing.
    ///
    /// The only `#[dagger::function]` in the fixture with a Void return.
    /// `#[dagger::check]` reaches the same encoder, but it is declared
    /// `is_check: true` and is run by `dagger check` rather than called, so
    /// nothing else here shows that a plain function may return nothing.
    #[dagger::function]
    pub fn returns_nothing(&self) {}

    /// Return nothing, spelled out.
    ///
    /// The other half of the void split, and it is a parsing difference rather
    /// than a stylistic one: with no `->` at all `parse_functions` leaves
    /// `return_ty` empty, and `-> ()` reaches `kind_of` as the text `()`. Both
    /// map to `VOID_KIND`, and only the empty form is compiled above.
    #[dagger::function]
    pub fn returns_unit(&self) -> () {}

    /// A check that passes.
    #[dagger::check]
    pub fn passing_check(&self) {}

    /// A check whose only argument carries a default, so it is still runnable.
    #[dagger::check]
    pub fn checked_with_default(&self, #[dagger(default = "ok")] mode: string) {
        if mode != "ok" {
            dagger::fail(string("checked_with_default got ") + mode)
        }
    }

    /// Write a marker file into the workspace.
    ///
    /// A generator: `dagger generate` runs it and applies what it returns. The
    /// `Workspace` argument is the one the engine injects itself, which is what
    /// lets a generator be called with nothing.
    #[dagger::function(generate)]
    pub fn regenerate(&self, ws: gen::Workspace) -> gen::Changeset {
        let before = ws.directory("/");
        let before_id = before.to_id().unwrap_or_else(|m| dagger::fail(m));

        before
            .with_new_file("e2e-generated.txt", "written by the e2e fixture generator")
            .changes(before_id)
    }

    // -------------------------------------------------------------------- objects

    /// List the fixture's test data, loaded from the module's context.
    ///
    /// Exercises `default_path` and `ignore` together: the caller passes
    /// nothing, the engine loads `testdata/` and drops `*.log` on the way.
    #[dagger::function]
    pub fn entries(
        &self,
        #[dagger(default_path = "testdata", ignore = ["*.log"])] dir: gen::Directory,
    ) -> string {
        self.join(dir.entries().unwrap_or_else(|m| dagger::fail(m)))
    }

    /// Count the entries of a directory the caller passes in.
    #[dagger::function]
    pub fn count_entries(&self, dir: gen::Directory) -> int {
        dir.entries().unwrap_or_else(|m| dagger::fail(m)).Len()
    }

    /// Return a container, as an object rather than as a string.
    #[dagger::function]
    pub fn container_out(&self, #[dagger(default = "alpine:3.22")] image: string) -> gen::Container {
        dag()
            .container()
            .from(image)
            .with_exec(&["echo", "-n", "container-out"])
    }

    // --------------------------------------------------------------------- client

    /// Read one scalar field over one round trip.
    #[dagger::function]
    pub fn leaf_string(&self) -> string {
        dag()
            .container()
            .from("alpine:3.22")
            .with_exec(&["echo", "-n", "leaf"])
            .stdout()
            .unwrap_or_else(|m| dagger::fail(m))
    }

    /// Decode an Int leaf.
    #[dagger::function]
    pub fn leaf_int(&self) -> int {
        dag()
            .directory()
            .with_new_file("a.txt", "xyz")
            .file("a.txt")
            .size()
            .unwrap_or_else(|m| dagger::fail(m))
    }

    /// Decode a Boolean leaf.
    #[dagger::function]
    pub fn leaf_bool(&self) -> bool {
        dag()
            .directory()
            .with_new_file("a.txt", "x")
            .exists("a.txt")
            .unwrap_or_else(|m| dagger::fail(m))
    }

    /// Decode a list of scalars.
    #[dagger::function]
    pub fn list_of_strings(&self) -> string {
        self.join(
            dag()
                .directory()
                .with_new_file("a.txt", "a")
                .with_new_file("b.txt", "b")
                .entries()
                .unwrap_or_else(|m| dagger::fail(m)),
        )
    }

    /// Decode a list of *objects*, and read a field off one of them.
    ///
    /// The list-of-object path is the hardest thing codegen emits: one round
    /// trip fetches the element ids, then each element is rebuilt on a fresh
    /// root chain through `loadEnvVariableFromID`. Nothing that asserts on
    /// generated text can show that the rebuilt object actually resolves.
    #[dagger::function]
    pub fn list_of_objects(&self) -> string {
        let vars = dag()
            .container()
            .from("alpine:3.22")
            .with_env_variable("E2E", "yes")
            .env_variables()
            .unwrap_or_else(|m| dagger::fail(m));

        let mut i: int = 0;
        while i < vars.Len() {
            if vars[i].name().unwrap_or_else(|m| dagger::fail(m)) == "E2E" {
                return vars[i].value().unwrap_or_else(|m| dagger::fail(m));
            }
            i += 1;
        }

        dagger::fail(string("E2E was not among the container's environment"))
    }

    /// Send an enum as an argument and decode one back.
    ///
    /// Covers `ToArg` (the value has to reach the query as a bare, unquoted
    /// literal), `FromJson`, an opts struct with a borrowed field, and the
    /// object-list rebuild, in one call.
    #[dagger::function]
    pub fn enum_round_trip(&self) -> string {
        let ports = dag()
            .container()
            .from("alpine:3.22")
            .with_exposed_port_opts(
                8080,
                &gen::ContainerWithExposedPortOpts {
                    protocol: Some(gen::NetworkProtocol::Udp),
                    ..Default::default()
                },
            )
            .exposed_ports()
            .unwrap_or_else(|m| dagger::fail(m));

        let mut i: int = 0;
        while i < ports.Len() {
            if ports[i].port().unwrap_or_else(|m| dagger::fail(m)) == 8080 {
                let protocol = ports[i].protocol().unwrap_or_else(|m| dagger::fail(m));
                return string(protocol.as_str());
            }
            i += 1;
        }

        dagger::fail(string("8080 was not among the container's exposed ports"))
    }

    /// Call both halves of a field that has optional arguments.
    #[dagger::function]
    pub fn opts_pair(&self) -> string {
        let dir = dag()
            .directory()
            .with_new_file("top.txt", "top")
            .with_new_file("nested/c.txt", "c");

        let all = self.join(dir.entries().unwrap_or_else(|m| dagger::fail(m)));
        let nested = self.join(
            dir.entries_opts(&gen::DirectoryEntriesOpts {
                path: Some("nested"),
                ..Default::default()
            })
            .unwrap_or_else(|m| dagger::fail(m)),
        );

        fmt::Sprintf!("%s|%s", all, nested)
    }

    /// Read several fields in one round trip.
    #[dagger::function]
    pub fn multi_field_fetch(&self) -> string {
        let (out, platform) = dag()
            .container()
            .from("alpine:3.22")
            .with_exec(&["echo", "-n", "leaf"])
            .fetch(|c| (c.stdout(), c.platform()))
            .unwrap_or_else(|m| dagger::fail(m));

        fmt::Sprintf!("%s|%s", out, platform)
    }

    /// Read a sub-selection, which restarts the positional alias counter.
    #[dagger::function]
    pub fn nested_select(&self) -> string {
        let (name, size) = dag()
            .container()
            .from("alpine:3.22")
            .fetch(|c| c.file("/etc/os-release").select(|f| (f.name(), f.size())))
            .unwrap_or_else(|m| dagger::fail(m));

        fmt::Sprintf!("%s|%d", name, size)
    }

    /// Chain across several object types before asking for a value.
    #[dagger::function]
    pub fn object_chain(&self) -> string {
        dag()
            .directory()
            .with_new_directory("a")
            .with_new_file("a/b.txt", "chained")
            .directory("a")
            .file("b.txt")
            .contents()
            .unwrap_or_else(|m| dagger::fail(m))
    }

    /// Hand an object back to a client method.
    ///
    /// It goes as an ID rather than as itself: the schema types the argument
    /// `DirectoryID`, which these bindings map to a string. See the SDK README.
    #[dagger::function]
    pub fn pass_object(&self) -> string {
        let dir = dag().directory().with_new_file("hello.txt", "mounted");
        let id = dir.to_id().unwrap_or_else(|m| dagger::fail(m));

        dag()
            .container()
            .from("alpine:3.22")
            .with_mounted_directory("/mnt", id)
            .with_exec(&["cat", "/mnt/hello.txt"])
            .stdout()
            .unwrap_or_else(|m| dagger::fail(m))
    }

    /// Join a list the engine returned, so a function can return it as a scalar.
    ///
    /// Unmarked, so it is invisible to `dagger call` and the macro never looks
    /// at its signature — which is just as well, because a `slice<string>`
    /// parameter is a compile error on anything the macro *does* declare.
    /// Lists being unsupported in a module signature is exactly why the
    /// list-typed *client* fields above are flattened before they are returned.
    fn join(&self, parts: slice<string>) -> string {
        strings::Join(parts, ",")
    }
}

#[goish::main]
fn main() {
    dagger::serve::<FeatureMatrix>()
}
