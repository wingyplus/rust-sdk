//! Test suite for the selection machinery.
//!
//! ```sh
//! cd sdk && cargo test
//! ```
//!
//! This is a `harness = false` target: libtest is `std`, and its `panic_impl`
//! collides with goish's, so there are no `#[test]` functions to collect. goish
//! ships Go's `testing` package instead, so the tests are ordinary functions
//! assembled into a list and handed to `testing::Main` — the same shape
//! `go test` generates — and cargo reads the exit status. Add a test to the
//! list in `main` or it never runs.
//!
//! Everything below the first divider is a hand-written stand-in for what
//! `sdk/codegen` will emit: four objects off the Dagger schema, spelled the way
//! the generator is meant to spell them. It is here to be *used* by the tests,
//! but it is also the point of the prototype — if the emitted shape were
//! awkward to write, that would show up here first.

#![no_std]
#![no_main]
#![allow(non_snake_case)]

use core::cell::RefCell;

use dagger::json_string;
use dagger::querybuilder::{Chain, Field, Fields, Leaf, ListField, OptField, Sel};
use dagger::engine::Transport;
use goish::encoding::json;
use goish::{bytes, fmt, int, nil, os, slice, string, testing};

// ─── the shape codegen will emit ──────────────────────────────────────

/// One rendered GraphQL argument list. Codegen emits this inline; here it
/// keeps the fixtures readable.
///
/// `string::from_bytes`, not `string(value)`: goish's `string(…)` conversion
/// takes a `&'static str`, and a generated setter's argument is borrowed from
/// the caller. Codegen has to spell it this way too.
fn arg(name: &str, value: &str) -> string {
    string("(") + name + ":" + json_string(&string::from_bytes(value.as_bytes())) + ")"
}

struct ContainerFields;

impl Fields for ContainerFields {
    fn new() -> Self {
        ContainerFields
    }
}

impl ContainerFields {
    fn stdout(&self) -> Leaf<string> {
        Leaf::new("stdout")
    }

    fn platform(&self) -> Leaf<string> {
        Leaf::new("platform")
    }

    fn entrypoint(&self) -> Leaf<slice<string>> {
        Leaf::new("entrypoint")
    }

    fn exit_code(&self) -> Leaf<int> {
        Leaf::new("exitCode")
    }

    /// A nullable scalar — `label(name: String!): String`.
    fn label(&self, name: &str) -> Leaf<Option<string>> {
        Leaf::with_args("label", arg("name", name))
    }

    fn file(&self, path: &str) -> Field<FileFields> {
        Field::with_args("file", arg("path", path))
    }

    fn env_variables(&self) -> ListField<EnvVariableFields> {
        ListField::new("envVariables")
    }
}

struct FileFields;

impl Fields for FileFields {
    fn new() -> Self {
        FileFields
    }
}

impl FileFields {
    fn contents(&self) -> Leaf<string> {
        Leaf::new("contents")
    }

    fn size(&self) -> Leaf<int> {
        Leaf::new("size")
    }

    /// A nullable object field, which is what makes depth 3 reachable.
    fn owner(&self) -> OptField<OwnerFields> {
        OptField::new("owner")
    }
}

struct OwnerFields;

impl Fields for OwnerFields {
    fn new() -> Self {
        OwnerFields
    }
}

impl OwnerFields {
    fn name(&self) -> Leaf<string> {
        Leaf::new("name")
    }

    fn uid(&self) -> Leaf<int> {
        Leaf::new("uid")
    }
}

struct EnvVariableFields;

impl Fields for EnvVariableFields {
    fn new() -> Self {
        EnvVariableFields
    }
}

impl EnvVariableFields {
    fn name(&self) -> Leaf<string> {
        Leaf::new("name")
    }

    fn value(&self) -> Leaf<string> {
        Leaf::new("value")
    }
}

// ─── render ───────────────────────────────────────────────────────────

/// The motivating example from the plan, rendered end to end through a chain.
fn TestRenderMatchesThePlannedDocument(t: &mut testing::T) {
    let chain = Chain::root()
        .field("container", string(""))
        .field("from", arg("address", "alpine"));

    let c = ContainerFields::new();
    let got = chain.render(&(
        c.stdout(),
        c.platform(),
        c.file("/etc/os-release").select(|f| (f.contents(), f.size())),
    ));

    let want = string(
        "{container{from(address:\"alpine\"){f0:stdout f1:platform f2:file(path:\"/etc/os-release\"){f0:contents f1:size}}}}",
    );
    if got != want {
        t.Error(fmt::Sprintf!("render =\n  %s\nwant\n  %s", got, want));
    }
}

/// The load-bearing rule: the alias counter restarts inside every `{ }` and
/// continues across siblings.
///
/// The selection is built to make a mistake in either half visible — a sub
/// nested in a sub, a leaf *after* that nested sub at the inner level, a
/// sibling list, and a sibling leaf after both. A counter that failed to
/// restart would number the inner fields `f2`/`f3`; one that failed to
/// continue would number `f2:size` as `f1`.
fn TestAliasCounterRestartsInsideBracesAndContinuesAcrossSiblings(t: &mut testing::T) {
    let c = ContainerFields::new();
    let selection = depth3(&c);

    let mut got = string("");
    let mut n: usize = 0;
    selection.render(&mut got, &mut n);

    let want = string(
        "f0:stdout f1:file(path:\"/etc/os-release\"){f0:contents f1:owner{f0:name f1:uid} f2:size} f2:envVariables{f0:name f1:value} f3:platform",
    );
    if got != want {
        t.Error(fmt::Sprintf!("render =\n  %s\nwant\n  %s", got, want));
    }
    if n != 4 {
        t.Error(fmt::Sprintf!("consumed %d aliases at the top level, want 4", n));
    }
}

/// The same field twice under different arguments is two leaves, and the
/// positional aliases keep them apart with no extra machinery.
fn TestSameFieldTwiceIsAliasedApart(t: &mut testing::T) {
    let c = ContainerFields::new();
    let selection = (c.label("org.opencontainers.version"), c.label("maintainer"));

    let mut got = string("");
    let mut n: usize = 0;
    selection.render(&mut got, &mut n);

    let want = string("f0:label(name:\"org.opencontainers.version\") f1:label(name:\"maintainer\")");
    if got != want {
        t.Error(fmt::Sprintf!("render = %q, want %q", got, want));
    }

    let response = parse(
        t,
        "{\"f0\":\"1.0\",\"f1\":null}",
    );
    let mut n: usize = 0;
    let (version, maintainer) = match selection.decode(&response, &mut n) {
        Ok(out) => out,
        Err(why) => t.Fatal(fmt::Sprintf!("decode: %s", why)),
    };
    if version != Some(string("1.0")) {
        t.Error(fmt::Sprintf!("version = %v, want 1.0", version.is_some()));
    }
    if maintainer.is_some() {
        t.Error("a null scalar decoded to Some");
    }
}

// ─── decode ───────────────────────────────────────────────────────────

/// Decode the depth-3 selection above against the response it would produce.
///
/// The `let` destructuring is the ergonomics claim in the plan; that it
/// type-checks at all is half the test.
fn TestDecodeRoundTripsAtDepth3(t: &mut testing::T) {
    let c = ContainerFields::new();
    let selection = depth3(&c);

    let response = parse(
        t,
        "{\"f0\":\"hi\\n\",\
          \"f1\":{\"f0\":\"NAME=alpine\",\"f1\":{\"f0\":\"root\",\"f1\":0},\"f2\":42},\
          \"f2\":[{\"f0\":\"PATH\",\"f1\":\"/bin\"},{\"f0\":\"HOME\",\"f1\":\"/root\"}],\
          \"f3\":\"linux/amd64\"}",
    );

    let mut n: usize = 0;
    let (out, (contents, owner, size), env, platform) = match selection.decode(&response, &mut n) {
        Ok(decoded) => decoded,
        Err(why) => t.Fatal(fmt::Sprintf!("decode: %s", why)),
    };

    assert_string(t, "stdout", out, "hi\n");
    assert_string(t, "contents", contents, "NAME=alpine");
    assert_string(t, "platform", platform, "linux/amd64");
    if size != 42 {
        t.Error(fmt::Sprintf!("size = %d, want 42", size));
    }

    match owner {
        Some((name, uid)) => {
            assert_string(t, "owner.name", name, "root");
            if uid != 0 {
                t.Error(fmt::Sprintf!("owner.uid = %d, want 0", uid));
            }
        }
        None => t.Error("owner decoded to None, want Some"),
    }

    if env.Len() != 2 {
        t.Fatal(fmt::Sprintf!("envVariables has %d entries, want 2", env.Len()));
    }
    // Every element decodes against the same selection, each from its own
    // alias 0 — the second entry is the one that catches a counter shared
    // across elements.
    assert_string(t, "envVariables[0].name", env[0].0.clone(), "PATH");
    assert_string(t, "envVariables[0].value", env[0].1.clone(), "/bin");
    assert_string(t, "envVariables[1].name", env[1].0.clone(), "HOME");
    assert_string(t, "envVariables[1].value", env[1].1.clone(), "/root");
}

/// A null object field is `None` for the whole sub-record, not a record of
/// `None`s — Dang's rule that a selection on a null receiver is null.
fn TestNullObjectDecodesToNone(t: &mut testing::T) {
    let c = ContainerFields::new();
    let selection = c
        .file("/x")
        .select(|f| (f.contents(), f.owner().select(|o| (o.name(), o.uid()))));

    let response = parse(t, "{\"f0\":{\"f0\":\"body\",\"f1\":null}}");
    let mut n: usize = 0;
    let (contents, owner) = match selection.decode(&response, &mut n) {
        Ok(decoded) => decoded,
        Err(why) => t.Fatal(fmt::Sprintf!("decode: %s", why)),
    };

    assert_string(t, "contents", contents, "body");
    if owner.is_some() {
        t.Error("a null object decoded to Some");
    }
}

/// An empty list is a list, not an error — and it exercises the capacity hint.
fn TestEmptyListDecodes(t: &mut testing::T) {
    let c = ContainerFields::new();
    let selection = c.env_variables().select(|e| (e.name(), e.value()));

    let response = parse(t, "{\"f0\":[]}");
    let mut n: usize = 0;
    let env = match selection.decode(&response, &mut n) {
        Ok(decoded) => decoded,
        Err(why) => t.Fatal(fmt::Sprintf!("decode: %s", why)),
    };
    if env.Len() != 0 {
        t.Error(fmt::Sprintf!("envVariables has %d entries, want 0", env.Len()));
    }
}

/// A list of scalars is a leaf, decoded by `FromJson for slice<T>` rather than
/// by `SubList` — the schema has 19 of these and no nested lists at all.
fn TestScalarListIsALeaf(t: &mut testing::T) {
    let c = ContainerFields::new();
    let selection = (c.entrypoint(), c.exit_code());

    let mut got = string("");
    let mut n: usize = 0;
    selection.render(&mut got, &mut n);
    let want = string("f0:entrypoint f1:exitCode");
    if got != want {
        t.Error(fmt::Sprintf!("render = %q, want %q", got, want));
    }

    let response = parse(t, "{\"f0\":[\"/bin/sh\",\"-c\"],\"f1\":0}");
    let mut n: usize = 0;
    let (entrypoint, code) = match selection.decode(&response, &mut n) {
        Ok(decoded) => decoded,
        Err(why) => t.Fatal(fmt::Sprintf!("decode: %s", why)),
    };
    if entrypoint.Len() != 2 {
        t.Fatal(fmt::Sprintf!("entrypoint has %d elements, want 2", entrypoint.Len()));
    }
    assert_string(t, "entrypoint[0]", entrypoint[0].clone(), "/bin/sh");
    assert_string(t, "entrypoint[1]", entrypoint[1].clone(), "-c");
    if code != 0 {
        t.Error(fmt::Sprintf!("exitCode = %d, want 0", code));
    }
}

// ─── the chain ────────────────────────────────────────────────────────

/// Chain steps are not aliased, so decoding walks the plain field names down
/// to the selection set.
fn TestChainWalksTheResponse(t: &mut testing::T) {
    let chain = Chain::root()
        .field("container", string(""))
        .field("from", arg("address", "alpine"));

    let c = ContainerFields::new();
    let selection = (c.stdout(), c.platform());

    let data = parse(
        t,
        "{\"container\":{\"from\":{\"f0\":\"hi\",\"f1\":\"linux/amd64\"}}}",
    );
    let (out, platform) = match chain.decode(&data, &selection) {
        Ok(decoded) => decoded,
        Err(why) => t.Fatal(fmt::Sprintf!("decode: %s", why)),
    };
    assert_string(t, "stdout", out, "hi");
    assert_string(t, "platform", platform, "linux/amd64");
}

/// Extending a chain leaves the receiver alone, so one object can be the
/// starting point for two different calls.
fn TestChainExtensionDoesNotMutateTheReceiver(t: &mut testing::T) {
    let base = Chain::root().field("container", string(""));
    let alpine = base.field("from", arg("address", "alpine"));
    let debian = base.field("from", arg("address", "debian"));

    let c = ContainerFields::new();
    assert_string(
        t,
        "alpine",
        alpine.render(&c.platform()),
        "{container{from(address:\"alpine\"){f0:platform}}}",
    );
    assert_string(
        t,
        "debian",
        debian.render(&c.platform()),
        "{container{from(address:\"debian\"){f0:platform}}}",
    );
    assert_string(
        t,
        "base",
        base.render(&c.platform()),
        "{container{f0:platform}}",
    );
}

// ─── the transport seam ───────────────────────────────────────────────

/// A `Transport` that answers from a fixture and records what it was asked.
///
/// The whole reason `fetch` takes a `Transport` rather than a `Session`: this
/// is the round trip a generated `X::fetch` performs, with no engine involved.
struct FakeTransport {
    reply: &'static str,
    sent: RefCell<string>,
}

impl Transport for FakeTransport {
    fn query(&self, document: &string) -> Result<json::Value, string> {
        *self.sent.borrow_mut() = document.clone();
        let raw = bytes(string(self.reply));
        let mut value = json::Value::Null;
        let err = json::Unmarshal(&raw, &mut value);
        if err != nil {
            return Err(string("fixture is not JSON"));
        }
        // A `Session` hands back the `data` object, so the fake does too.
        match value.AsObject() {
            Some(object) => {
                let (data, ok) = object.Get("data");
                if !ok {
                    return Err(string("fixture has no data"));
                }
                Ok(data)
            }
            None => Err(string("fixture is not an object")),
        }
    }
}

/// `fetch` is render, send, decode — and nothing else.
fn TestFetchRoundTripsThroughATransport(t: &mut testing::T) {
    let chain = Chain::root()
        .field("container", string(""))
        .field("from", arg("address", "alpine"));

    let c = ContainerFields::new();
    let engine = FakeTransport {
        reply: "{\"data\":{\"container\":{\"from\":{\"f0\":\"hi\",\"f1\":\"linux/amd64\"}}}}",
        sent: RefCell::new(string("")),
    };

    let (out, platform) = match dagger::fetch(&engine, &chain, &(c.stdout(), c.platform())) {
        Ok(decoded) => decoded,
        Err(why) => t.Fatal(fmt::Sprintf!("fetch: %s", why)),
    };

    assert_string(
        t,
        "the document sent",
        engine.sent.borrow().clone(),
        "{container{from(address:\"alpine\"){f0:stdout f1:platform}}}",
    );
    assert_string(t, "stdout", out, "hi");
    assert_string(t, "platform", platform, "linux/amd64");
}

/// A transport failure comes back as-is rather than being rewrapped, so the
/// message a caller sees is the one the session produced.
fn TestFetchPropagatesATransportFailure(t: &mut testing::T) {
    struct Broken;
    impl Transport for Broken {
        fn query(&self, _document: &string) -> Result<json::Value, string> {
            Err(string("querying the engine session: connection refused"))
        }
    }

    let c = ContainerFields::new();
    match dagger::fetch(&Broken, &Chain::root(), &c.stdout()) {
        Ok(_) => t.Error("fetch succeeded against a broken transport"),
        Err(why) => assert_contains(t, "transport failure", why, "connection refused"),
    }
}

// ─── failure shapes ───────────────────────────────────────────────────

/// A response missing a selected alias names the alias, and a scalar of the
/// wrong type names the field — the two ways a schema change breaks a caller.
fn TestDecodeErrorsNameWhatWentWrong(t: &mut testing::T) {
    let c = ContainerFields::new();

    let selection = (c.stdout(), c.platform());
    let response = parse(t, "{\"f0\":\"hi\"}");
    let mut n: usize = 0;
    match selection.decode(&response, &mut n) {
        Ok(_) => t.Error("decoding a response missing f1 succeeded"),
        Err(why) => assert_contains(t, "missing alias", why, "f1"),
    }

    let selection = c.exit_code();
    let response = parse(t, "{\"f0\":\"not a number\"}");
    let mut n: usize = 0;
    match selection.decode(&response, &mut n) {
        Ok(_) => t.Error("decoding a string into an int succeeded"),
        Err(why) => assert_contains(t, "wrong scalar type", why, "exitCode"),
    }

    // A nested failure is prefixed by the path it was found under, so the
    // message says which sub-selection went wrong rather than just "expected a
    // number".
    let selection = c.file("/x").select(|f| (f.contents(), f.size()));
    let response = parse(t, "{\"f0\":{\"f0\":\"body\",\"f1\":\"huge\"}}");
    let mut n: usize = 0;
    match selection.decode(&response, &mut n) {
        Ok(_) => t.Error("decoding a string into a nested int succeeded"),
        Err(why) => {
            assert_contains(t, "nested failure", why.clone(), "file");
            assert_contains(t, "nested failure", why, "size");
        }
    }
}

// ─── helpers ──────────────────────────────────────────────────────────

/// The depth-3 selection both the render and the decode test are built on:
/// container → file → owner, with siblings at every level.
fn depth3(
    c: &ContainerFields,
) -> impl Sel<
    Out = (
        string,
        (string, Option<(string, int)>, int),
        slice<(string, string)>,
        string,
    ),
> {
    (
        c.stdout(),
        c.file("/etc/os-release").select(|f| {
            (
                f.contents(),
                f.owner().select(|o| (o.name(), o.uid())),
                f.size(),
            )
        }),
        c.env_variables().select(|e| (e.name(), e.value())),
        c.platform(),
    )
}

fn parse(t: &testing::T, text: &'static str) -> json::Value {
    let raw = bytes(string(text));
    let mut value = json::Value::Null;
    let err = json::Unmarshal(&raw, &mut value);
    if err != nil {
        t.Fatal(fmt::Sprintf!("parsing the fixture: %v", err));
    }
    value
}

fn assert_string(t: &testing::T, what: &'static str, got: string, want: &'static str) {
    if got != string(want) {
        t.Error(fmt::Sprintf!("%s = %q, want %q", what, got, want));
    }
}

fn assert_contains(t: &testing::T, what: &'static str, got: string, want: &'static str) {
    if !goish::strings::Contains(got.clone(), want) {
        t.Error(fmt::Sprintf!("%s: %q does not mention %q", what, got, want));
    }
}

#[goish::main]
fn main() {
    let tests: &[(&str, testing::TestFn)] = &[
        (
            "TestRenderMatchesThePlannedDocument",
            TestRenderMatchesThePlannedDocument,
        ),
        (
            "TestAliasCounterRestartsInsideBracesAndContinuesAcrossSiblings",
            TestAliasCounterRestartsInsideBracesAndContinuesAcrossSiblings,
        ),
        (
            "TestSameFieldTwiceIsAliasedApart",
            TestSameFieldTwiceIsAliasedApart,
        ),
        ("TestDecodeRoundTripsAtDepth3", TestDecodeRoundTripsAtDepth3),
        ("TestNullObjectDecodesToNone", TestNullObjectDecodesToNone),
        ("TestEmptyListDecodes", TestEmptyListDecodes),
        ("TestScalarListIsALeaf", TestScalarListIsALeaf),
        ("TestChainWalksTheResponse", TestChainWalksTheResponse),
        (
            "TestChainExtensionDoesNotMutateTheReceiver",
            TestChainExtensionDoesNotMutateTheReceiver,
        ),
        (
            "TestFetchRoundTripsThroughATransport",
            TestFetchRoundTripsThroughATransport,
        ),
        (
            "TestFetchPropagatesATransportFailure",
            TestFetchPropagatesATransportFailure,
        ),
        (
            "TestDecodeErrorsNameWhatWentWrong",
            TestDecodeErrorsNameWhatWentWrong,
        ),
    ];
    os::Exit(testing::Main(tests));
}
