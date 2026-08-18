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
use dagger::{Arguments, EnumType, Object};
use goish::encoding::json;
use goish::{append, bytes, fmt, int, make, nil, os, slice, string, testing};

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

/// Not a Dagger object: the four shapes GraphQL can spell for a list with a
/// scalar element type.
///
/// The schema has only the first today — all 19 of its scalar lists are
/// `[Scalar!]!` — so the other three are insurance rather than current
/// coverage. They are worth pinning anyway: none of them is written anywhere
/// in the crate, they exist purely as compositions of the blanket
/// `FromJson for slice<T>` and `FromJson for Option<T>` impls, so a change to
/// either could break them while `entrypoint` above keeps passing.
struct ListShapeFields;

impl Fields for ListShapeFields {
    fn new() -> Self {
        ListShapeFields
    }
}

impl ListShapeFields {
    /// `[String!]!`
    fn required(&self) -> Leaf<slice<string>> {
        Leaf::new("required")
    }

    /// `[String!]`
    fn nullable_list(&self) -> Leaf<Option<slice<string>>> {
        Leaf::new("nullableList")
    }

    /// `[String]!`
    fn nullable_items(&self) -> Leaf<slice<Option<string>>> {
        Leaf::new("nullableItems")
    }

    /// `[String]`
    fn both_nullable(&self) -> Leaf<Option<slice<Option<string>>>> {
        Leaf::new("bothNullable")
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

/// A step narrowed by an inline fragment opens a second brace in the query.
///
/// This is how an object is rebuilt from its id: the engine dropped the
/// per-type `loadXFromID` loaders for one Relay-style `node`, which is typed as
/// an interface whose only field is `id`, so anything else needs `... on X`.
fn TestInlineFragmentNarrowsANodeStep(t: &mut testing::T) {
    let chain = Chain::root().field_on("node", arg("id", "ctr-1"), "Container");

    let c = ContainerFields::new();
    let got = chain.render(&(c.stdout(), c.platform()));

    let want =
        string("{node(id:\"ctr-1\"){... on Container{f0:stdout f1:platform}}}");
    if got != want {
        t.Error(fmt::Sprintf!("render =\n  %s\nwant\n  %s", got, want));
    }
}

/// An inline fragment adds a brace to the query and no level to the response.
///
/// The server answers a narrowed selection under the field's own name, so
/// decoding walks `node` and stops — an extra hop for the fragment would look
/// for a key that is never there.
fn TestInlineFragmentAddsNoResponseLevel(t: &mut testing::T) {
    let chain = Chain::root().field_on("node", arg("id", "ctr-1"), "Container");

    let c = ContainerFields::new();
    let data = parse(t, "{\"node\":{\"f0\":\"hi\",\"f1\":\"linux/amd64\"}}");
    let (out, platform) = match chain.decode(&data, &(c.stdout(), c.platform())) {
        Ok(decoded) => decoded,
        Err(why) => t.Fatal(fmt::Sprintf!("decode: %s", why)),
    };
    assert_string(t, "stdout", out, "hi");
    assert_string(t, "platform", platform, "linux/amd64");
}

/// A fragment step composes with ordinary ones, in both directions.
///
/// A rebuilt object is a chain like any other, so a caller goes on selecting
/// from it — and every brace the fragment opened still has to be closed.
fn TestInlineFragmentComposesWithPlainSteps(t: &mut testing::T) {
    let chain = Chain::root()
        .field_on("node", arg("id", "ctr-1"), "Container")
        .field("from", arg("address", "alpine"));

    let c = ContainerFields::new();
    let got = chain.render(&c.stdout());
    let want = string("{node(id:\"ctr-1\"){... on Container{from(address:\"alpine\"){f0:stdout}}}}");
    if got != want {
        t.Error(fmt::Sprintf!("render =\n  %s\nwant\n  %s", got, want));
    }

    let data = parse(t, "{\"node\":{\"from\":{\"f0\":\"hi\"}}}");
    match chain.decode(&data, &c.stdout()) {
        Ok(out) => assert_string(t, "stdout", out, "hi"),
        Err(why) => t.Fatal(fmt::Sprintf!("decode: %s", why)),
    }
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

/// All four list nullability shapes, off one fixture.
///
/// A nullable *list* is `None`; a nullable *element* is a `None` inside the
/// list. Confusing the two is the mistake this catches — the middle two rows
/// decode from JSON that is a list either way.
fn TestScalarListNullabilityShapes(t: &mut testing::T) {
    let l = ListShapeFields::new();
    let selection = (
        l.required(),
        l.nullable_list(),
        l.nullable_items(),
        l.both_nullable(),
    );

    // A scalar list renders like any other leaf: no braces, because there is
    // no sub-selection to make.
    let mut got = string("");
    let mut n: usize = 0;
    selection.render(&mut got, &mut n);
    assert_string(
        t,
        "render",
        got,
        "f0:required f1:nullableList f2:nullableItems f3:bothNullable",
    );

    let response = parse(
        t,
        "{\"f0\":[\"a\",\"b\"],\"f1\":null,\"f2\":[\"x\",null],\"f3\":[null]}",
    );
    let mut n: usize = 0;
    let (required, nullable_list, nullable_items, both_nullable) =
        match selection.decode(&response, &mut n) {
            Ok(decoded) => decoded,
            Err(why) => t.Fatal(fmt::Sprintf!("decode: %s", why)),
        };

    // [String!]! — a plain list.
    if required.Len() != 2 {
        t.Fatal(fmt::Sprintf!("required has %d elements, want 2", required.Len()));
    }
    assert_string(t, "required[0]", required[0].clone(), "a");
    assert_string(t, "required[1]", required[1].clone(), "b");

    // [String!] — the list itself is absent.
    if nullable_list.is_some() {
        t.Error("a null list decoded to Some");
    }

    // [String]! — the list is there; one element is not.
    if nullable_items.Len() != 2 {
        t.Fatal(fmt::Sprintf!(
            "nullableItems has %d elements, want 2",
            nullable_items.Len()
        ));
    }
    match nullable_items[0].clone() {
        Some(value) => assert_string(t, "nullableItems[0]", value, "x"),
        None => t.Error("nullableItems[0] decoded to None, want Some"),
    }
    if nullable_items[1].is_some() {
        t.Error("a null element decoded to Some");
    }

    // [String] — present, holding one absent element. A `Some` wrapping a list
    // of `None`, which is the shape most easily collapsed by mistake.
    match both_nullable {
        Some(items) => {
            if items.Len() != 1 {
                t.Fatal(fmt::Sprintf!(
                    "bothNullable has %d elements, want 1",
                    items.Len()
                ));
            }
            if items[0].is_some() {
                t.Error("bothNullable[0] decoded to Some, want None");
            }
        }
        None => t.Error("bothNullable decoded to None, want Some"),
    }
}

/// A bad element says *which* element. The field name alone does not, and a
/// list is the one place that matters.
fn TestListElementErrorsNameTheIndex(t: &mut testing::T) {
    // A scalar list: the index comes from `FromJson for slice<T>`, under the
    // field name the enclosing Leaf supplies.
    let c = ContainerFields::new();
    let response = parse(t, "{\"f0\":[\"/bin/sh\",7]}");
    let mut n: usize = 0;
    match c.entrypoint().decode(&response, &mut n) {
        Ok(_) => t.Error("decoding a number into a string list succeeded"),
        Err(why) => {
            assert_contains(t, "scalar list", why.clone(), "entrypoint");
            assert_contains(t, "scalar list", why, "[1]");
        }
    }

    // An object list: the index comes from `SubList`, and the failing inner
    // field is named after it.
    let selection = c.env_variables().select(|e| (e.name(), e.value()));
    let response = parse(
        t,
        "{\"f0\":[{\"f0\":\"PATH\",\"f1\":\"/bin\"},{\"f0\":\"HOME\",\"f1\":42}]}",
    );
    let mut n: usize = 0;
    match selection.decode(&response, &mut n) {
        Ok(_) => t.Error("decoding a number into a string field succeeded"),
        Err(why) => {
            assert_contains(t, "object list", why.clone(), "envVariables[1]");
            assert_contains(t, "object list", why, "value");
        }
    }
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

// ─── enums a module declares ──────────────────────────────────────────

/// An enum a module might declare.
///
/// The attribute is the point as much as the type is: `sdk/macros`'s own suite
/// builds its values by hand, because the `proc_macro` API panics outside a
/// macro expansion, so an actual `enum` going through `#[dagger::enum_type]` is
/// only ever compiled here and in the end-to-end fixture.
#[dagger::enum_type]
enum TargetOs {
    /// Alpine Linux.
    Alpine,
    /// Debian.
    Debian,
}

/// The `inputArgs` of a call, as `serve` hands them over: each value is still
/// the JSON *text* the engine encoded it into.
fn arguments(entries: &[(&'static str, &'static str)]) -> Arguments {
    let mut pairs = make!([](string, string), 0, entries.len() as int);
    for (name, value) in entries {
        pairs = append!(pairs, (string(*name), string(*value)));
    }
    Arguments::new(pairs)
}

/// An enum argument arrives as one member's name — not as an ID, the way an
/// object does — and an absent optional as nothing at all.
fn TestEnumArgumentsArriveAsAMemberName(t: &mut testing::T) {
    let args = arguments(&[("os", "\"Alpine\""), ("fallback", "null"), ("count", "3")]);

    match args.enum_member("os") {
        Ok(member) => assert_string(t, "the member name", member, "Alpine"),
        Err(why) => t.Error(fmt::Sprintf!("reading an enum argument: %s", why)),
    }

    match args.enum_member_opt("fallback") {
        Ok(None) => {}
        Ok(Some(member)) => t.Error(fmt::Sprintf!("an absent optional read as %q", member)),
        Err(why) => t.Error(fmt::Sprintf!("reading an absent optional: %s", why)),
    }

    // The two ways it can be wrong, and both name the argument: the engine
    // supplies an enum the module declared, so either is the two sides
    // disagreeing rather than a caller's mistake.
    match args.enum_member("missing") {
        Ok(member) => t.Error(fmt::Sprintf!("a missing argument read as %q", member)),
        Err(why) => assert_contains(t, "a missing enum argument", why, "missing"),
    }
    match args.enum_member("count") {
        Ok(member) => t.Error(fmt::Sprintf!("a number read as the member %q", member)),
        Err(why) => assert_contains(t, "a number as an enum", why, "enum member"),
    }
}

/// A member crosses the boundary as its name in both directions, spelled as the
/// variant is: `from_member` reads what an argument carried, `encode_enum`
/// writes what a return carries, and the two agree.
fn TestEnumMembersRoundTripThroughTheirNames(t: &mut testing::T) {
    let os = match TargetOs::from_member(&string("Debian")) {
        Ok(os) => os,
        Err(why) => t.Fatal(fmt::Sprintf!("from_member: %s", why)),
    };
    assert_string(t, "the member", string(os.member()), "Debian");
    assert_string(t, "the encoded member", dagger::encode_enum(&os), "\"Debian\"");

    // The engine's own spelling of the member — what a caller writes — is not
    // what a module deals in, and reading one back would be accepting a name
    // this side never declared.
    match TargetOs::from_member(&string("DEBIAN")) {
        Ok(_) => t.Error("the schema's spelling of a member was accepted"),
        Err(why) => assert_contains(t, "an unknown member", why, "DEBIAN"),
    }
}

/// What `register` is handed for an enum: the type's name and doc comment, and
/// every variant with its own.
fn TestAnEnumDeclaresItsMembers(t: &mut testing::T) {
    let def = TargetOs::DEF;
    assert_string(t, "the enum name", string(def.name), "TargetOs");
    assert_contains(t, "the enum doc", string(def.doc), "An enum a module might declare.");

    if def.members.len() != 2 {
        t.Fatal(fmt::Sprintf!("declared %d members, want 2", def.members.len() as int));
    }
    assert_string(t, "the first member", string(def.members[0].name), "Alpine");
    assert_string(t, "its doc", string(def.members[0].doc), "Alpine Linux.");
    assert_string(t, "the second member", string(def.members[1].name), "Debian");
}

/// A module that declares an enum, as a user writes one.
///
/// Compiled here rather than only in the end-to-end fixture because this is
/// what the two halves of the declaration meeting looks like: the enum above
/// says what its members are, `enums(TargetOs)` says the module serves it, and
/// what `#[dagger::object]` emits for a signature naming it has to build and
/// dispatch. Everything below reaches it without an engine, since neither the
/// table nor the dispatch is a round trip.
///
/// It carries no state, which is why the attribute on the `struct` has nothing
/// to read; it is still required, because that is the half of the declaration
/// that says how the receiver is rebuilt. See `tests/state.rs`.
#[dagger::object]
struct EnumModule;

#[dagger::object(enums(TargetOs))]
impl EnumModule {
    /// Return the OS unchanged.
    #[dagger::function]
    pub fn echo_os(&self, os: TargetOs) -> TargetOs {
        os
    }

    /// Report which libc an OS carries, or that none was chosen.
    #[dagger::function]
    pub fn os_libc(&self, os: Option<TargetOs>) -> string {
        match os {
            Some(TargetOs::Alpine) => string("musl"),
            Some(TargetOs::Debian) => string("glibc"),
            None => string("unset"),
        }
    }
}

/// The enum reaches the table `register` walks: once as the module's own
/// declaration, and by name in the signature that uses it.
fn TestAModuleDeclaresTheEnumsItServes(t: &mut testing::T) {
    let enums = <EnumModule as Object>::enums();
    if enums.len() != 1 {
        t.Fatal(fmt::Sprintf!("declared %d enums, want 1", enums.len() as int));
    }
    assert_string(t, "the declared enum", string(enums[0].name), "TargetOs");

    let functions = <EnumModule as Object>::functions();
    let mut found = false;
    for def in functions {
        if def.name != "echoOs" {
            continue;
        }
        found = true;
        assert_string(t, "the return kind", string(def.return_kind), "ENUM_KIND");
        assert_string(t, "the return type", string(def.return_type_name), "TargetOs");
        if def.args.len() != 1 {
            t.Fatal(fmt::Sprintf!("echoOs declared %d arguments, want 1", def.args.len() as int));
        }
        assert_string(t, "the argument kind", string(def.args[0].kind), "ENUM_KIND");
        assert_string(t, "the argument type", string(def.args[0].type_name), "TargetOs");
    }
    if !found {
        t.Error("echoOs is not among the declared functions");
    }
}

/// The dispatch reads a member name into a variant and writes one back — and
/// says so when the name is not one the enum has.
fn TestDispatchTurnsAMemberNameIntoAVariant(t: &mut testing::T) {
    match EnumModule.invoke(&string("echoOs"), &arguments(&[("os", "\"Debian\"")])) {
        Ok(encoded) => assert_string(t, "the encoded return", encoded, "\"Debian\""),
        Err(why) => t.Error(fmt::Sprintf!("echoOs: %s", why)),
    }

    // The optional one, supplied and omitted: what the function matched on was
    // a variant either way.
    match EnumModule.invoke(&string("osLibc"), &arguments(&[("os", "\"Alpine\"")])) {
        Ok(encoded) => assert_string(t, "a supplied optional", encoded, "\"musl\""),
        Err(why) => t.Error(fmt::Sprintf!("osLibc: %s", why)),
    }
    match EnumModule.invoke(&string("osLibc"), &arguments(&[("os", "null")])) {
        Ok(encoded) => assert_string(t, "an omitted optional", encoded, "\"unset\""),
        Err(why) => t.Error(fmt::Sprintf!("osLibc: %s", why)),
    }

    match EnumModule.invoke(&string("echoOs"), &arguments(&[("os", "\"PLAN9\"")])) {
        Ok(encoded) => t.Error(fmt::Sprintf!("an unknown member dispatched to %s", encoded)),
        Err(why) => assert_contains(t, "an unknown member", why, "PLAN9"),
    }
}

/// What a `None` return encodes to is what an absent argument arrives as, and
/// the two have to be the same three characters.
///
/// The engine hands an optional argument to a module as the JSON text `null` and
/// reads the module's result back as a JSON document, so both ends of an
/// `Option` cross the boundary through the same spelling. `encode_null` writing
/// anything else — the *string* `"null"`, an empty document — would be a call
/// that fails rather than a value that is absent, and it would fail inside the
/// engine rather than here.
///
/// The one encoder in this suite: the rest of the dispatch is what
/// `#[dagger::object]` emits, and `sdk/macros`'s own tests assert on that text.
fn TestNullEncodesTheWayAnAbsentArgumentArrives(t: &mut testing::T) {
    let encoded = dagger::encode_null();
    assert_string(t, "encode_null", encoded.clone(), "null");

    // Decoded rather than only compared, so the assertion is that those
    // characters *are* JSON null. The starting value is a non-null one: `Value`
    // defaults to `Null`, so a decode that wrote nothing at all would otherwise
    // look like a pass.
    let mut value = json::Value::Bool(true);
    let err = json::Unmarshal(&bytes(encoded.clone()), &mut value);
    if err != nil {
        t.Fatal(fmt::Sprintf!("encode_null is not a JSON document: %v", err));
    }
    if !value.IsNull() {
        t.Error("encode_null decoded to something other than JSON null");
    }

    // And back in through the argument side, which is where the engine's own
    // `null` lands: the same text a module writes for `None` reads back as one.
    let args = dagger::Arguments::new(slice!([](string, string) {
        (string("maybe"), encoded)
    }));
    match args.string_opt("maybe") {
        Ok(None) => {}
        Ok(Some(got)) => t.Error(fmt::Sprintf!("an encoded null read back as %q", got)),
        Err(why) => t.Error(fmt::Sprintf!("an encoded null failed to read back: %s", why)),
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
        (
            "TestScalarListNullabilityShapes",
            TestScalarListNullabilityShapes,
        ),
        (
            "TestListElementErrorsNameTheIndex",
            TestListElementErrorsNameTheIndex,
        ),
        ("TestChainWalksTheResponse", TestChainWalksTheResponse),
        (
            "TestInlineFragmentNarrowsANodeStep",
            TestInlineFragmentNarrowsANodeStep,
        ),
        (
            "TestInlineFragmentAddsNoResponseLevel",
            TestInlineFragmentAddsNoResponseLevel,
        ),
        (
            "TestInlineFragmentComposesWithPlainSteps",
            TestInlineFragmentComposesWithPlainSteps,
        ),
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
        (
            "TestEnumArgumentsArriveAsAMemberName",
            TestEnumArgumentsArriveAsAMemberName,
        ),
        (
            "TestEnumMembersRoundTripThroughTheirNames",
            TestEnumMembersRoundTripThroughTheirNames,
        ),
        ("TestAnEnumDeclaresItsMembers", TestAnEnumDeclaresItsMembers),
        (
            "TestAModuleDeclaresTheEnumsItServes",
            TestAModuleDeclaresTheEnumsItServes,
        ),
        (
            "TestDispatchTurnsAMemberNameIntoAVariant",
            TestDispatchTurnsAMemberNameIntoAVariant,
        ),
        (
            "TestNullEncodesTheWayAnAbsentArgumentArrives",
            TestNullEncodesTheWayAnAbsentArgumentArrives,
        ),
    ];
    os::Exit(testing::Main(tests));
}
