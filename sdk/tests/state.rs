//! Test suite for an object's state, its constructor, and dispatch onto both.
//!
//! ```sh
//! cd sdk && cargo test
//! ```
//!
//! A `harness = false` target like `tests/querybuilder.rs`, and for the same
//! reason — see its header. Add a test to the list in `main` or it never runs.
//!
//! What is different here is that the subject is what `#[dagger::object]`
//! *emits*: the object below is declared exactly as a module's would be, so
//! this file is the one place in the repository where the macro's output is
//! compiled and then run. `sdk/macros`' own suite can only assert on the text,
//! and it builds its `Function` values by hand because the `proc_macro` API
//! panics outside an expansion.
//!
//! The gap it does not close is the engine: nothing here sends a query, so an
//! object *field* is exercised through `Workspace`, which is an ID wrapper with
//! no client behind it. The end-to-end suite in `.dagger/modules/tests` is what
//! puts the same declarations in front of a real one.

#![no_std]
#![no_main]
#![allow(non_snake_case)]

use dagger::{Arguments, Object, ObjectId, ObjectState, State, Workspace};
use goish::{fmt, int, make, os, slice, string, strings, testing};

// ─── the object under test ────────────────────────────────────────────

/// An enum the module declares, carried as one of this object's fields.
#[dagger::enum_type]
pub enum Mode {
    /// Build for release.
    Release,
    /// Build for debugging.
    Debug,
}

/// A module's root object, with one field of every kind that fits in one.
///
/// `#[dagger::object]` on the `struct` is what makes the fields state: it emits
/// the `ObjectState` impl that encodes them into the document the engine keeps,
/// and decodes them back into the receiver on the next call.
///
/// It repeats `enums(Mode)` from the `impl` block below, and has to: the
/// attribute sees one item at a time, so nothing else tells the `struct` half
/// that `Mode` is an enum this module declares rather than an engine object it
/// has never heard of.
#[dagger::object(enums(Mode))]
pub struct Config {
    /// The image every step runs in.
    pub image: string,
    /// How many jobs to run at once.
    pub jobs: int,
    /// How much to scale by.
    pub factor: f64,
    /// Whether to say more.
    pub loud: bool,
    /// What to build for.
    pub mode: Mode,
    /// The tags to publish under.
    pub tags: slice<string>,
    /// The tag to publish under, if any.
    pub tag: Option<string>,
    /// The workspace this was configured against, if any.
    pub workspace: Option<Workspace>,
    /// Superseded by `tag`.
    #[dagger(deprecated = "use tag instead")]
    pub label: Option<string>,
}

/// A configured build.
#[dagger::object(enums(Mode))]
impl Config {
    /// Configure the build.
    #[dagger::constructor]
    pub fn new(
        #[dagger(default = "alpine:3.22")] image: string,
        #[dagger(default = 2)] jobs: int,
        #[dagger(default = 1.5)] factor: f64,
        #[dagger(default = false)] loud: bool,
        tag: Option<string>,
        tags: Option<slice<string>>,
    ) -> Config {
        Config {
            image,
            jobs,
            factor,
            loud,
            mode: Mode::Release,
            tags: tags.unwrap_or_else(|| make!([]string, 0, 0)),
            tag,
            workspace: None,
            label: None,
        }
    }

    /// Describe the configuration in one line.
    #[dagger::function]
    pub fn describe(&self) -> string {
        fmt::Sprintf!(
            "%s/%d/%v",
            self.image.clone(),
            self.jobs,
            self.tag.clone().unwrap_or(string("untagged"))
        )
    }

    /// Reconfigure the number of jobs.
    ///
    /// Takes `self` by value, which is the ordinary shape of a builder and the
    /// reason `Object::invoke` owns its receiver: the object belongs to the one
    /// call the engine decoded it for.
    #[dagger::function]
    pub fn with_jobs(self, jobs: int) -> Self {
        Config { jobs, ..self }
    }

    /// The number of jobs, doubled.
    ///
    /// An associated function rather than a method, so the dispatch reaches it
    /// through the type rather than through the receiver.
    #[dagger::function]
    pub fn double(n: int) -> int {
        n * 2
    }
}

/// A module with no state at all — the shape every scaffolded module starts in.
///
/// Here to be *compiled*: the unit path through the macro emits an empty field
/// table, a receiver rebuilt from nothing and a writer nothing is written to,
/// and a warning in any of those would appear in every new module rather than
/// here.
#[dagger::object]
pub struct Bare;

/// A module with nothing to configure.
#[dagger::object]
impl Bare {
    /// Say hello.
    #[dagger::function]
    pub fn hello(&self) -> string {
        string("hello")
    }
}

// ─── fields ───────────────────────────────────────────────────────────

/// Every `pub` field is declared to the engine, camelCased, in the order it was
/// written — which is also the order `to_state` writes them, so the encoded
/// document and the registered type describe each other.
#[allow(non_snake_case)]
fn TestFieldsAreDeclaredInDeclarationOrder(t: &mut testing::T) {
    let fields = <Config as ObjectState>::fields();
    // Name, kind, list, optional — the same four a signature's argument is
    // mapped onto, since a field and an argument of one type are the same
    // declaration seen from either side of the call.
    let want: &[(&str, &str, bool, bool)] = &[
        ("image", "STRING_KIND", false, false),
        ("jobs", "INTEGER_KIND", false, false),
        ("factor", "FLOAT_KIND", false, false),
        ("loud", "BOOLEAN_KIND", false, false),
        ("mode", "ENUM_KIND", false, false),
        ("tags", "STRING_KIND", true, false),
        ("tag", "STRING_KIND", false, true),
        ("workspace", "OBJECT_KIND", false, true),
        ("label", "STRING_KIND", false, true),
    ];

    if fields.len() != want.len() {
        t.Fatal(fmt::Sprintf!(
            "declared %d fields, want %d",
            fields.len() as int,
            want.len() as int
        ));
    }
    for (i, (name, kind, list, optional)) in want.iter().enumerate() {
        let got = &fields[i];
        if got.name != *name || got.kind != *kind || got.list != *list || got.optional != *optional {
            t.Error(fmt::Sprintf!(
                "field %d is %s/%s, want %s/%s",
                i as int,
                got.name,
                got.kind,
                *name,
                *kind
            ));
        }
    }
    if fields[4].type_name != "Mode" {
        t.Error(fmt::Sprintf!(
            "mode names the enum %q, want %q",
            fields[4].type_name,
            "Mode"
        ));
    }
    if fields[7].type_name != "Workspace" {
        t.Error(fmt::Sprintf!(
            "workspace names the object %q, want %q",
            fields[7].type_name,
            "Workspace"
        ));
    }
    if fields[0].doc != "The image every step runs in." {
        t.Error(fmt::Sprintf!("image's doc comment is %q", fields[0].doc));
    }
    if fields[8].deprecated != "use tag instead" {
        t.Error(fmt::Sprintf!("label's deprecation is %q", fields[8].deprecated));
    }
}

// ─── the round trip ───────────────────────────────────────────────────

/// The document the engine keeps decodes back into the object it was written
/// from — which is the whole of what "state" means here.
#[allow(non_snake_case)]
fn TestStateRoundTripsThroughItsDocument(t: &mut testing::T) {
    let before = Config {
        image: string("rust:1.90"),
        jobs: 4,
        factor: 0.5,
        loud: true,
        mode: Mode::Debug,
        tags: slice!([]string { "v1", "latest" }),
        tag: Some(string("v1")),
        workspace: Some(Workspace::from_id(string("ws-id"))),
        label: None,
    };

    let document = match before.to_state() {
        Ok(document) => document,
        Err(why) => t.Fatal(fmt::Sprintf!("encoding: %s", why)),
    };
    assert_string(
        t,
        "the encoded document",
        document.clone(),
        r#"{"image":"rust:1.90","jobs":4,"factor":0.5,"loud":true,"mode":"Debug","tags":["v1","latest"],"tag":"v1","workspace":"ws-id","label":null}"#,
    );

    let state = match State::decode(&document) {
        Ok(state) => state,
        Err(why) => t.Fatal(fmt::Sprintf!("decoding: %s", why)),
    };
    let after = match Config::from_state(&state) {
        Ok(after) => after,
        Err(why) => t.Fatal(fmt::Sprintf!("rebuilding: %s", why)),
    };

    assert_string(t, "image", after.image, "rust:1.90");
    if after.jobs != 4 {
        t.Error(fmt::Sprintf!("jobs = %d, want 4", after.jobs));
    }
    if after.factor != 0.5 {
        t.Error(fmt::Sprintf!("factor = %v, want 0.5", after.factor));
    }
    if !after.loud {
        t.Error(string("loud did not survive the round trip"));
    }
    assert_string(t, "tag", after.tag.unwrap_or(string("<none>")), "v1");
    match after.workspace {
        Some(ws) => assert_string(t, "workspace", ws.id(), "ws-id"),
        None => t.Error(string("the workspace did not survive the round trip")),
    }
    if after.label.is_some() {
        t.Error(string("an absent optional came back present"));
    }
}

/// A required field nothing supplied is an error naming it, rather than a
/// silent zero: Rust has no zero value to fall back on, and a module that
/// received one would fail somewhere further along.
#[allow(non_snake_case)]
fn TestAMissingFieldIsAnErrorNamingIt(t: &mut testing::T) {
    let state = match State::decode(&string("{}")) {
        Ok(state) => state,
        Err(why) => t.Fatal(fmt::Sprintf!("decoding: %s", why)),
    };
    match Config::from_state(&state) {
        Ok(_) => t.Error(string("an empty document rebuilt an object with required fields")),
        Err(why) => {
            assert_contains(t, "the message", why.clone(), "image");
            assert_contains(t, "the message", why, "field");
        }
    }
}

/// A field of the wrong type is an error too, and it says what was expected.
#[allow(non_snake_case)]
fn TestAFieldOfTheWrongTypeIsAnError(t: &mut testing::T) {
    let state = match State::decode(&string(r#"{"image":"x","jobs":"four"}"#)) {
        Ok(state) => state,
        Err(why) => t.Fatal(fmt::Sprintf!("decoding: %s", why)),
    };
    match Config::from_state(&state) {
        Ok(_) => t.Error(string("a string decoded as an integer field")),
        Err(why) => assert_contains(t, "the message", why, "not an integer"),
    }
}

/// The empty state is what a top-level call carries, and what a module with no
/// constructor is built from: a type with no fields is happy with it, and one
/// with fields is not.
#[allow(non_snake_case)]
fn TestTheEmptyStateReadsEveryFieldAsAbsent(t: &mut testing::T) {
    let state = State::empty();
    match state.string_opt("image") {
        Ok(None) => {}
        Ok(Some(_)) => t.Error(string("the empty state produced a value")),
        Err(why) => t.Error(fmt::Sprintf!("the empty state failed: %s", why)),
    }
}

/// A module with no state declares none, encodes to an empty document, and is
/// built from one — which is what `Object::construct` falls back to when no
/// constructor was declared.
#[allow(non_snake_case)]
fn TestAModuleWithNoStateRoundTripsAnEmptyDocument(t: &mut testing::T) {
    if !<Bare as ObjectState>::fields().is_empty() {
        t.Error(string("a unit struct declared fields"));
    }
    if Bare::constructor().is_some() {
        t.Error(string("a module with no constructor declared one"));
    }
    match Bare::construct(&Arguments::new(slice!([](string, string) {}))) {
        Ok(document) => assert_string(t, "the constructed state", document, "{}"),
        Err(why) => t.Error(fmt::Sprintf!("constructing: %s", why)),
    }

    let state = match State::decode(&string("{}")) {
        Ok(state) => state,
        Err(why) => t.Fatal(fmt::Sprintf!("decoding: %s", why)),
    };
    let bare = match Bare::from_state(&state) {
        Ok(bare) => bare,
        Err(why) => t.Fatal(fmt::Sprintf!("rebuilding: %s", why)),
    };
    match bare.invoke(&string("hello"), &Arguments::new(slice!([](string, string) {}))) {
        Ok(value) => assert_string(t, "hello", value, "\"hello\""),
        Err(why) => t.Error(fmt::Sprintf!("hello: %s", why)),
    }
}

// ─── the constructor ──────────────────────────────────────────────────

/// The constructor is registered under the empty name, which is how the engine
/// spells "this function builds the object" — `new` is a Rust convention and
/// the API has no such function.
#[allow(non_snake_case)]
fn TestTheConstructorIsRegisteredWithNoName(t: &mut testing::T) {
    let def = match Config::constructor() {
        Some(def) => def,
        None => t.Fatal(string("no constructor was registered")),
    };
    if !def.name.is_empty() {
        t.Error(fmt::Sprintf!("the constructor is named %q, want no name", def.name));
    }
    if def.return_type_name != "Config" {
        t.Error(fmt::Sprintf!(
            "the constructor returns %q, want %q",
            def.return_type_name,
            "Config"
        ));
    }
    if def.args.len() != 6 {
        t.Error(fmt::Sprintf!(
            "the constructor takes %d arguments, want 6",
            def.args.len() as int
        ));
    }
    if def.args[0].default_value != "\"alpine:3.22\"" {
        t.Error(fmt::Sprintf!(
            "image's default is %s",
            def.args[0].default_value
        ));
    }
    // It is not one of the object's own functions: the engine reaches it
    // through the type def rather than by name, so listing it in both places
    // would declare a function called `new` as well.
    for f in Config::functions() {
        if f.name == "new" {
            t.Error(string("the constructor is also declared as a function"));
        }
    }
}

/// Running the constructor produces the document the engine will hand back as
/// `parent` on the next call.
#[allow(non_snake_case)]
fn TestConstructingEncodesTheObjectsState(t: &mut testing::T) {
    let args = Arguments::new(slice!([](string, string) {
        (string("image"), string("\"rust:1.90\"")),
        (string("jobs"), string("8")),
        (string("factor"), string("2")),
        (string("loud"), string("true")),
        (string("tag"), string("null")),
    }));

    match Config::construct(&args) {
        Ok(document) => assert_string(
            t,
            "the constructed state",
            document,
            r#"{"image":"rust:1.90","jobs":8,"factor":2,"loud":true,"mode":"Release","tags":[],"tag":null,"workspace":null,"label":null}"#,
        ),
        Err(why) => t.Error(fmt::Sprintf!("constructing: %s", why)),
    }
}

// ─── dispatch ─────────────────────────────────────────────────────────

/// A function reads the state the receiver arrived with, rather than arguments
/// repeating it.
#[allow(non_snake_case)]
fn TestInvokeReadsTheReceiversState(t: &mut testing::T) {
    let config = configured();
    match config.invoke(&string("describe"), &Arguments::new(slice!([](string, string) {}))) {
        Ok(value) => assert_string(t, "describe", value, "\"rust:1.90/4/v1\""),
        Err(why) => t.Error(fmt::Sprintf!("describe: %s", why)),
    }
}

/// A function returning the object's own type hands back its *state* rather
/// than an id: the engine holds no value to mint one for, so what it keeps is
/// the document the fields encode to.
#[allow(non_snake_case)]
fn TestReturningTheObjectEncodesItsState(t: &mut testing::T) {
    let args = Arguments::new(slice!([](string, string) {
        (string("jobs"), string("16")),
    }));

    match configured().invoke(&string("withJobs"), &args) {
        Ok(document) => {
            assert_contains(t, "the returned state", document.clone(), "\"jobs\":16");
            assert_contains(t, "the returned state", document, "\"image\":\"rust:1.90\"");
        }
        Err(why) => t.Error(fmt::Sprintf!("withJobs: %s", why)),
    }
}

/// An associated function is reached through the type, so it dispatches on an
/// object whose state it never looks at.
#[allow(non_snake_case)]
fn TestAnAssociatedFunctionDispatchesWithoutTheReceiver(t: &mut testing::T) {
    let args = Arguments::new(slice!([](string, string) {
        (string("n"), string("21")),
    }));

    match configured().invoke(&string("double"), &args) {
        Ok(value) => assert_string(t, "double", value, "42"),
        Err(why) => t.Error(fmt::Sprintf!("double: %s", why)),
    }
}

/// A name the object does not serve is an error rather than a panic.
#[allow(non_snake_case)]
fn TestAnUnknownFunctionIsRefused(t: &mut testing::T) {
    let args = Arguments::new(slice!([](string, string) {}));
    match configured().invoke(&string("nope"), &args) {
        Ok(_) => t.Error(string("an unknown function was dispatched")),
        Err(why) => assert_contains(t, "the message", why, "no such function"),
    }
}

// ─── helpers ──────────────────────────────────────────────────────────

fn configured() -> Config {
    Config {
        image: string("rust:1.90"),
        jobs: 4,
        factor: 1.0,
        loud: false,
        mode: Mode::Release,
        tags: make!([]string, 0, 0),
        tag: Some(string("v1")),
        workspace: None,
        label: None,
    }
}

fn assert_string(t: &mut testing::T, what: &'static str, got: string, want: &'static str) {
    if got != string(want) {
        t.Error(fmt::Sprintf!("%s = %q, want %q", what, got, want));
    }
}

fn assert_contains(t: &mut testing::T, what: &'static str, got: string, want: &'static str) {
    if !strings::Contains(got.clone(), want) {
        t.Error(fmt::Sprintf!("%s: %q does not mention %q", what, got, want));
    }
}

#[goish::main]
fn main() {
    let tests: &[(&str, testing::TestFn)] = &[
        (
            "TestFieldsAreDeclaredInDeclarationOrder",
            TestFieldsAreDeclaredInDeclarationOrder,
        ),
        (
            "TestStateRoundTripsThroughItsDocument",
            TestStateRoundTripsThroughItsDocument,
        ),
        (
            "TestAMissingFieldIsAnErrorNamingIt",
            TestAMissingFieldIsAnErrorNamingIt,
        ),
        (
            "TestAFieldOfTheWrongTypeIsAnError",
            TestAFieldOfTheWrongTypeIsAnError,
        ),
        (
            "TestTheEmptyStateReadsEveryFieldAsAbsent",
            TestTheEmptyStateReadsEveryFieldAsAbsent,
        ),
        (
            "TestAModuleWithNoStateRoundTripsAnEmptyDocument",
            TestAModuleWithNoStateRoundTripsAnEmptyDocument,
        ),
        (
            "TestTheConstructorIsRegisteredWithNoName",
            TestTheConstructorIsRegisteredWithNoName,
        ),
        (
            "TestConstructingEncodesTheObjectsState",
            TestConstructingEncodesTheObjectsState,
        ),
        (
            "TestInvokeReadsTheReceiversState",
            TestInvokeReadsTheReceiversState,
        ),
        (
            "TestReturningTheObjectEncodesItsState",
            TestReturningTheObjectEncodesItsState,
        ),
        (
            "TestAnAssociatedFunctionDispatchesWithoutTheReceiver",
            TestAnAssociatedFunctionDispatchesWithoutTheReceiver,
        ),
        (
            "TestAnUnknownFunctionIsRefused",
            TestAnUnknownFunctionIsRefused,
        ),
    ];
    os::Exit(testing::Main(tests));
}
