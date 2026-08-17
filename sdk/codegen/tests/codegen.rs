//! Test suite for the bindings generator.
//!
//! ```sh
//! cd sdk/codegen && cargo test
//! ```
//!
//! This is a `harness = false` target: libtest is `std`, and its `panic_impl`
//! collides with goish's, so there are no `#[test]` functions to collect. goish
//! ships Go's `testing` package instead, so the tests are ordinary functions
//! assembled into a list and handed to `testing::Main` — the same shape
//! `go test` generates — and cargo reads the exit status. Add a test to the
//! list in `main` or it never runs.
//!
//! Most of what follows renders [`FIXTURE`], a miniature schema, and looks for
//! exact lines in the output. Asserting on text rather than on a parse tree is
//! the point: the generator's contract is the source it writes, and the shapes
//! being pinned — a shadowed local, a doc comment with no item under it — are
//! ones that show up as a compile error in a *module*, days later, rather than
//! anywhere near here.
//!
//! What this cannot prove is that the whole emitted surface compiles. That
//! needs the engine's real schema, which is 1.2 MB of JSON and not something to
//! vendor; generate against a live engine and build the crate with the result
//! in place of `sdk/src/gen/mod.rs` when changing what is emitted.

#![no_std]
#![no_main]
#![allow(non_snake_case)]

use codegen::names::{enum_variant, escape_ident, snake_case, upper_first};
use codegen::{generate, schema};
use goish::{append, bytes, fmt, make, nil, os, slice, string, strings, testing};

// ─── name conversions ────────────────────────────────────────────────

/// Field and argument names become snake_case, and an acronym is one word.
///
/// The acronym cases are the ones a naive per-uppercase-letter split gets
/// wrong: `withGPU` has to be `with_gpu`, not `with_g_p_u`, and `HTTPState`
/// breaks before the `S` rather than after the first `T`.
fn TestSnakeCase(t: &mut testing::T) {
    const CASES: &[(&str, &str)] = &[
        ("stdout", "stdout"),
        ("withExec", "with_exec"),
        ("asService", "as_service"),
        ("withGPU", "with_gpu"),
        ("HTTPState", "http_state"),
        ("asJSON", "as_json"),
        ("foo2Bar", "foo2_bar"),
        ("already_snake", "already_snake"),
        ("_DirectiveApplication", "directive_application"),
        ("id", "id"),
    ];

    for &(name, want) in CASES {
        let got = snake_case(&string(name));
        if got != string(want) {
            t.Error(fmt::Sprintf!("snake_case(%q) = %q, want %q", name, got, want));
        }
    }
}

/// A name landing on a Rust keyword is escaped rather than mangled — as a raw
/// identifier where that works, and with a trailing underscore for the four
/// keywords `r#` cannot rescue.
fn TestEscapesRustKeywords(t: &mut testing::T) {
    const CASES: &[(&str, &str)] = &[
        ("ref", "r#ref"),
        ("loop", "r#loop"),
        ("enum", "r#enum"),
        ("type", "r#type"),
        ("self", "self_"),
        ("crate", "crate_"),
        ("Self", "Self_"),
        ("super", "super_"),
        ("stdout", "stdout"),
    ];

    for &(name, want) in CASES {
        let got = escape_ident(string(name));
        if got != string(want) {
            t.Error(fmt::Sprintf!("escape_ident(%q) = %q, want %q", name, got, want));
        }
    }
}

fn TestUpperFirst(t: &mut testing::T) {
    const CASES: &[(&str, &str)] = &[
        ("withExec", "WithExec"),
        ("id", "Id"),
        ("withGPU", "WithGPU"),
        ("", ""),
    ];

    for &(name, want) in CASES {
        let got = upper_first(&string(name));
        if got != string(want) {
            t.Error(fmt::Sprintf!("upper_first(%q) = %q, want %q", name, got, want));
        }
    }
}

/// `SCREAMING_SNAKE` folds to CamelCase, and a value that is already mixed-case
/// is left as its author wrote it.
///
/// The collision rule is the interesting half. `ImageLayerCompression` really
/// does hold both `Gzip` and `GZIP`, neither deprecated, and folding both would
/// declare the same variant twice. When a folded name is not unique across the
/// enum, every value sharing it keeps its schema spelling — which is decided
/// from the whole value set, so the answer does not depend on the order the
/// values arrive in.
fn TestEnumVariantNaming(t: &mut testing::T) {
    let plain = names(&["SHARED", "PRIVATE", "LOCKED"]);
    expect_variant(t, "SHARED", &plain, "Shared");
    expect_variant(t, "LOCKED", &plain, "Locked");

    let mixed = names(&["Default", "PerSession", "Never"]);
    expect_variant(t, "PerSession", &mixed, "PerSession");

    let colliding = names(&["Gzip", "GZIP", "EStarGZ", "ESTARGZ", "Uncompressed", "UNCOMPRESSED"]);
    expect_variant(t, "Gzip", &colliding, "Gzip");
    expect_variant(t, "GZIP", &colliding, "GZIP");
    // `EStarGZ` keeps its own spelling and `ESTARGZ` folds to `Estargz`: the
    // two do not collide, so neither needs the fallback.
    expect_variant(t, "EStarGZ", &colliding, "EStarGZ");
    expect_variant(t, "ESTARGZ", &colliding, "Estargz");
    expect_variant(t, "UNCOMPRESSED", &colliding, "UNCOMPRESSED");

    // Order must not decide the answer.
    let reversed = names(&["UNCOMPRESSED", "Uncompressed", "ESTARGZ", "EStarGZ", "GZIP", "Gzip"]);
    expect_variant(t, "Gzip", &reversed, "Gzip");
    expect_variant(t, "GZIP", &reversed, "GZIP");

    // A folded name landing on a keyword is still escaped.
    let keyword = names(&["SELF", "OTHER"]);
    expect_variant(t, "SELF", &keyword, "Self_");
}

fn names(values: &[&'static str]) -> slice<string> {
    let mut out = make!([]string, 0, values.len() as goish::int);
    for value in values {
        out = append!(out, string(*value));
    }
    out
}

fn expect_variant(t: &mut testing::T, value: &'static str, all: &slice<string>, want: &'static str) {
    let got = enum_variant(&string(value), all);
    if got != string(want) {
        t.Error(fmt::Sprintf!("enum_variant(%q) = %q, want %q", value, got, want));
    }
}

// ─── parsing ─────────────────────────────────────────────────────────

/// Both shapes the engine hands out parse: the bare `{"__schema": …}` that
/// `introspectionSchemaJSON` returns, and the `{"data": {"__schema": …}}` a raw
/// GraphQL reply is wrapped in.
fn TestParseAcceptsBothResponseShapes(t: &mut testing::T) {
    let (bare, err) = schema::parse_string(&string(FIXTURE));
    if err != nil {
        t.Fatal(fmt::Sprintf!("parse bare: %v", err));
    }
    if bare.query_type != "Query" {
        t.Error(fmt::Sprintf!("query type = %q", bare.query_type));
    }

    let wrapped = string("{\"data\":") + string(FIXTURE) + "}";
    let (found, err) = schema::parse_string(&wrapped);
    if err != nil {
        t.Fatal(fmt::Sprintf!("parse wrapped: %v", err));
    }
    if found.types.Len() != bare.types.Len() {
        t.Error(fmt::Sprintf!(
            "wrapped has %d types, bare has %d",
            found.types.Len(),
            bare.types.Len()
        ));
    }
}

/// A wrapper chain is flattened to the three things the renderer wants, and
/// nothing else: which named type, whether it is a list, whether it is
/// nullable.
fn TestFlattensTypeReferences(t: &mut testing::T) {
    let (parsed, err) = schema::parse_string(&string(FIXTURE));
    if err != nil {
        t.Fatal(fmt::Sprintf!("parse: %v", err));
    }
    let container = match parsed.find(&string("Container")) {
        Some(found) => found,
        None => {
            t.Fatal("no Container in the fixture");
        }
    };

    // `String!`
    let stdout = container.field("stdout").unwrap();
    if !stdout.ty.non_null || stdout.ty.list || stdout.ty.name != "String" {
        t.Error("stdout is not a non-null String");
    }
    // `String` — nullable
    let env = container.field("envVariable").unwrap();
    if env.ty.non_null {
        t.Error("envVariable is not nullable");
    }
    // `[String!]!`
    let entries = container.field("entries").unwrap();
    if !entries.ty.non_null || !entries.ty.list || entries.ty.name != "String" {
        t.Error("entries is not a non-null list of String");
    }
    // The argument's own wrappers, and its kind.
    let with_exec = container.field("withExec").unwrap();
    if with_exec.ty.kind != "OBJECT" {
        t.Error(fmt::Sprintf!("withExec returns kind %q", with_exec.ty.kind));
    }
    if with_exec.args.Len() != 4 {
        t.Error(fmt::Sprintf!("withExec has %d args", with_exec.args.Len()));
    }
    if !with_exec.args[0].ty.list || !with_exec.args[0].ty.non_null {
        t.Error("withExec's args argument is not a required list");
    }
}

fn TestParseRejectsARepsonseWithoutASchema(t: &mut testing::T) {
    let (_, err) = schema::parse_string(&string("{\"nothing\":true}"));
    if err == nil {
        t.Error("parsing a response with no __schema succeeded");
    }
}

// ─── rendering ───────────────────────────────────────────────────────

fn render_fixture(t: &mut testing::T) -> string {
    let (source, err) = generate(&bytes(string(FIXTURE)));
    if err != nil {
        t.Fatal(fmt::Sprintf!("generate: %v", err));
    }
    source
}

/// A scalar field sends a query; an object field extends the chain.
fn TestLeafAndChainMethods(t: &mut testing::T) {
    let source = render_fixture(t);

    contains(t, &source, "pub struct Container {");
    contains(t, &source, "    transport: Arc<dyn Transport>,");
    contains(t, &source, "    q: Chain,");

    contains(t, &source, "    pub fn stdout(&self) -> Result<string, string> {");
    contains(
        t,
        &source,
        "engine::fetch(&*self.transport, &self.q, &Leaf::<string>::new(\"stdout\"))",
    );

    // Nullable scalar, so the leaf decodes through an `Option`.
    contains(
        t,
        &source,
        "pub fn env_variable(&self, name: impl Into<string>) -> Result<Option<string>, string> {",
    );

    // An object field costs no round trip: it appends a step and hands back the
    // target, carrying the same transport.
    contains(t, &source, "    pub fn file(&self, path: impl Into<string>) -> File {");
    contains(
        t,
        &source,
        "File::new(self.transport.clone(), self.q.field(\"file\", __args.finish()))",
    );

    // A field with no arguments renders as a bare name, not as `()`.
    contains(
        t,
        &source,
        "Service::new(self.transport.clone(), self.q.field(\"asService\", string(\"\")))",
    );

    // The docs the schema carries survive into the generated docs.
    contains(t, &source, "    /// The command's standard output.");
}

/// The local holding the argument list cannot be named `args`, because a schema
/// field can have an argument named `args` — `withExec` does — and the local
/// would shadow the parameter. `arg_list(args)` would then be handed the
/// half-built argument list rather than the caller's command.
fn TestArgumentNamedArgsIsNotShadowed(t: &mut testing::T) {
    let source = render_fixture(t);

    contains(t, &source, "    pub fn with_exec(&self, args: &[&str]) -> Container {");
    contains(t, &source, "        let mut __args = Args::new();");
    contains(t, &source, "        __args.put(\"args\", arg_list(args));");
    excludes(t, &source, "let mut args = Args::new();");
}

/// Nullable arguments become a struct that derives `Default`, and a value left
/// `None` is left out of the query entirely so the engine applies its own
/// default.
fn TestOptionalArgumentsBecomeAnOptsStruct(t: &mut testing::T) {
    let source = render_fixture(t);

    contains(t, &source, "pub struct ContainerWithExecOpts<'a> {");
    contains(t, &source, "    pub stdin: Option<&'a str>,");
    contains(t, &source, "    pub expand: Option<bool>,");
    contains(t, &source, "    pub compression: Option<Compression>,");
    contains(t, &source, "#[derive(Clone, Default)]");

    contains(
        t,
        &source,
        "    pub fn with_exec_opts(&self, args: &[&str], opts: &ContainerWithExecOpts<'_>) -> Container {",
    );
    contains(t, &source, "        if let Some(value) = opts.stdin {");
    contains(t, &source, "            __args.put(\"stdin\", arg_string(value));");

    // The plain form forwards to the opts form rather than duplicating it.
    contains(
        t,
        &source,
        "        self.with_exec_opts(args, &ContainerWithExecOpts::default())",
    );

    // An opts struct of nothing but copyable values must not declare a
    // lifetime: an unused lifetime parameter does not compile.
    contains(t, &source, "pub struct ContainerROpts {");
    contains(t, &source, "    pub fn r_opts(&self, opts: &ContainerROpts) -> Result<string, string> {");

    // A schema argument on a Rust keyword is escaped in the struct too.
    contains(t, &source, "    pub r#enum: Option<bool>,");
    contains(t, &source, "        if let Some(value) = opts.r#enum {");
}

/// The same fields, as nodes of a multi-field selection. Nullability picks the
/// builder: `Field` for a non-null object, `OptField` for a nullable one,
/// `ListField` for a list.
fn TestFieldsNamespace(t: &mut testing::T) {
    let source = render_fixture(t);

    contains(t, &source, "pub struct ContainerFields;");
    contains(t, &source, "impl Fields for ContainerFields {");
    contains(t, &source, "    pub fn stdout(&self) -> Leaf<string> {");
    contains(t, &source, "        Leaf::<string>::new(\"stdout\")");
    contains(t, &source, "    pub fn file(&self, path: impl Into<string>) -> Field<FileFields> {");
    contains(t, &source, "    pub fn as_service(&self) -> OptField<ServiceFields> {");
    contains(t, &source, "    pub fn crumbs(&self) -> ListField<CrumbFields> {");
    // A list of scalars is a leaf, not a `ListField`: there is no sub-selection
    // to make on a string.
    contains(t, &source, "    pub fn entries(&self) -> Leaf<slice<string>> {");
}

/// A list of objects is fetched as ids and rebuilt through the element type's
/// loader, because no chain points at element three.
fn TestListOfObjectsGoesThroughTheLoader(t: &mut testing::T) {
    let source = render_fixture(t);

    contains(t, &source, "    pub fn crumbs(&self) -> Result<slice<Crumb>, string> {");
    contains(t, &source, "            &ListField::<CrumbFields>::new(\"crumbs\").select(|f| f.id()),");
    contains(
        t,
        &source,
        "Crumb::new(self.transport.clone(), Chain::root().field(\"loadCrumbFromID\", __id.finish()))",
    );
}

/// An element type with no loader is the one shape that is skipped — and the
/// note saying so has to be a plain comment. A `///` with no item under it
/// attaches itself to whatever method comes next, which silently documents the
/// wrong thing.
fn TestListWithoutALoaderIsSkipped(t: &mut testing::T) {
    let source = render_fixture(t);

    excludes(t, &source, "slice<Orphan>");
    contains(t, &source, "    // `orphans` is a list of `Orphan`, which has no loader");
    excludes(t, &source, "    /// Not available as a method");
}

/// An object with a loader implements `ObjectId`, which is what lets a module
/// function take it as an argument or hand it back.
///
/// The two directions are asymmetric and both are pinned here: `from_id` starts
/// a fresh chain at the loader over the process's own session, since an
/// argument arrives with nothing else attached, while `to_id` runs the chain
/// the object already carries.
fn TestObjectsCrossTheBoundaryById(t: &mut testing::T) {
    let source = render_fixture(t);

    contains(t, &source, "impl crate::ObjectId for Container {");
    contains(t, &source, "    fn from_id(id: string) -> Container {");
    contains(t, &source, "        __id.put(\"id\", arg_string(id));");
    contains(
        t,
        &source,
        "            Chain::root().field(\"loadContainerFromID\", __id.finish()),",
    );
    contains(t, &source, "            engine::default_transport(),");
    contains(t, &source, "    fn to_id(&self) -> Result<string, string> {");
    contains(
        t,
        &source,
        "        engine::fetch(&*self.transport, &self.q, &Leaf::<string>::new(\"id\"))",
    );
}

/// A type the engine has no loader for cannot be rebuilt from an id, so it does
/// not implement the trait — and the note saying so has to be a plain comment,
/// for the reason [`TestListWithoutALoaderIsSkipped`] gives.
fn TestObjectWithoutALoaderIsNotAnObjectId(t: &mut testing::T) {
    let source = render_fixture(t);

    // `File` has an `id` field and no loader; `Query` has neither.
    excludes(t, &source, "impl crate::ObjectId for File {");
    excludes(t, &source, "impl crate::ObjectId for Query {");
    contains(t, &source, "// `File` has no loader taking an id");
    excludes(t, &source, "/// `File` has no loader taking an id");
}

/// Enums carry their schema spelling in both directions, and a GraphQL enum
/// literal is spliced unquoted.
fn TestEnums(t: &mut testing::T) {
    let source = render_fixture(t);

    contains(t, &source, "pub enum Compression {");
    contains(t, &source, "    Gzip,");
    contains(t, &source, "    GZIP,");
    contains(t, &source, "    Zstd,");
    contains(t, &source, "    PerFile,");

    contains(t, &source, "            Compression::PerFile => \"PER_FILE\",");
    contains(t, &source, "impl ToArg for Compression {");
    contains(t, &source, "        string(self.as_str())");
    contains(t, &source, "impl FromJson for Compression {");
    contains(t, &source, "        if found == \"PER_FILE\" {");
}

/// An input object is a value the caller builds, so its fields are owned; it
/// renders as a `{…}` literal rather than as a `(…)` argument list.
fn TestInputObjects(t: &mut testing::T) {
    let source = render_fixture(t);

    contains(t, &source, "pub struct BuildArg {");
    contains(t, &source, "    pub name: string,");
    contains(t, &source, "    pub value: Option<string>,");
    contains(t, &source, "impl ToArg for BuildArg {");
    // Borrowed, not moved: `to_arg` takes `&self`.
    contains(t, &source, "        __args.put(\"name\", arg_string(&self.name));");
    contains(t, &source, "        __args.object()");
    // And as an argument, a list of them goes through `arg_list`.
    contains(t, &source, "    pub build_args: Option<&'a [BuildArg]>,");
    contains(t, &source, "            __args.put(\"buildArgs\", arg_list(value));");
}

/// Scalars get no type of their own — every one but `Void` is text, and the
/// schema hands them back as plain `ID` — so `Platform` decodes as a `string`.
fn TestScalarsAreText(t: &mut testing::T) {
    let source = render_fixture(t);

    contains(t, &source, "    pub fn platform(&self) -> Result<string, string> {");
    contains(t, &source, "    pub fn id(&self) -> Result<string, string> {");
    excludes(t, &source, "pub struct Platform");
    excludes(t, &source, "pub struct ContainerID");
    // `Void` is the exception, and decodes from the null it arrives as.
    contains(t, &source, "pub struct Void;");
    contains(t, &source, "impl FromJson for Void {");
}

/// A deprecated field is still emitted, with the engine's reason attached.
fn TestDeprecatedFields(t: &mut testing::T) {
    let source = render_fixture(t);

    contains(t, &source, "    #[deprecated(note = \"Use stdout instead.\")]");
    contains(t, &source, "    /// Deprecated: Use stdout instead.");
}

/// Introspection's own types are not part of a module's API.
fn TestMetaTypesAreNotEmitted(t: &mut testing::T) {
    let source = render_fixture(t);
    excludes(t, &source, "pub struct __Type");
}

/// The root is reached through two free functions: the no-argument one a
/// module calls, and the one taking a transport that it is built on — which is
/// where a caller-supplied transport enters the generated surface.
fn TestRootFunction(t: &mut testing::T) {
    let source = render_fixture(t);
    contains(t, &source, "pub fn dag() -> Query {");
    contains(t, &source, "    dag_with(engine::default_transport())");
    contains(t, &source, "pub fn dag_with(transport: Arc<dyn Transport>) -> Query {");
    contains(t, &source, "    Query::new(transport, Chain::root())");
}

/// No `Sprintf` verb went unfilled.
///
/// `fmt::Sprintf!` is a `macro_rules!` that hands its format string to
/// `sprintf_impl` at run time, so a wrong argument count is not a compile
/// error: Go's formatter writes `%!s(MISSING)` or `%!(EXTRA …)` into the output
/// and carries on. That artifact would land in somebody's `mod.rs` and fail to
/// compile a long way from here, naming nothing useful. Every such marker
/// starts `%!`, so one assertion catches all of them.
fn TestNoFormatArtifacts(t: &mut testing::T) {
    let source = render_fixture(t);
    excludes(t, &source, "%!");
}

/// The same schema always renders the same source: types are emitted in name
/// order, and nothing here depends on how the engine happened to serialise it.
fn TestOutputIsDeterministic(t: &mut testing::T) {
    let first = render_fixture(t);
    let second = render_fixture(t);
    if first != second {
        t.Error("two runs over the same schema disagreed");
    }
    let container = strings::Index(first.clone(), "pub struct Container ");
    let file = strings::Index(first.clone(), "pub struct File ");
    if container < 0 || file < 0 || container > file {
        t.Error("types are not emitted in name order");
    }
}

// ─── assertions ──────────────────────────────────────────────────────

fn contains(t: &mut testing::T, source: &string, want: &'static str) {
    if !strings::Contains(source.clone(), want) {
        t.Error(fmt::Sprintf!("generated source is missing:\n  %s", want));
    }
}

fn excludes(t: &mut testing::T, source: &string, unwanted: &'static str) {
    if strings::Contains(source.clone(), unwanted) {
        t.Error(fmt::Sprintf!("generated source should not contain:\n  %s", unwanted));
    }
}

// ─── the fixture ─────────────────────────────────────────────────────

/// A miniature introspection schema, shaped like the engine's.
///
/// Every case worth pinning is in here and nothing else is: an argument named
/// `args`, a field on a Rust keyword, an argument on one, an enum with
/// case-variant aliases, a list of objects with a loader and one without, an
/// input object, an acronym in a field name, and a deprecated field.
const FIXTURE: &str = include_str!("fixture.json");

#[goish::main]
fn main() {
    let tests: &[(&str, testing::TestFn)] = &[
        ("TestSnakeCase", TestSnakeCase),
        ("TestEscapesRustKeywords", TestEscapesRustKeywords),
        ("TestUpperFirst", TestUpperFirst),
        ("TestEnumVariantNaming", TestEnumVariantNaming),
        (
            "TestParseAcceptsBothResponseShapes",
            TestParseAcceptsBothResponseShapes,
        ),
        ("TestFlattensTypeReferences", TestFlattensTypeReferences),
        (
            "TestParseRejectsARepsonseWithoutASchema",
            TestParseRejectsARepsonseWithoutASchema,
        ),
        ("TestLeafAndChainMethods", TestLeafAndChainMethods),
        (
            "TestArgumentNamedArgsIsNotShadowed",
            TestArgumentNamedArgsIsNotShadowed,
        ),
        (
            "TestOptionalArgumentsBecomeAnOptsStruct",
            TestOptionalArgumentsBecomeAnOptsStruct,
        ),
        ("TestFieldsNamespace", TestFieldsNamespace),
        (
            "TestListOfObjectsGoesThroughTheLoader",
            TestListOfObjectsGoesThroughTheLoader,
        ),
        (
            "TestListWithoutALoaderIsSkipped",
            TestListWithoutALoaderIsSkipped,
        ),
        (
            "TestObjectsCrossTheBoundaryById",
            TestObjectsCrossTheBoundaryById,
        ),
        (
            "TestObjectWithoutALoaderIsNotAnObjectId",
            TestObjectWithoutALoaderIsNotAnObjectId,
        ),
        ("TestEnums", TestEnums),
        ("TestInputObjects", TestInputObjects),
        ("TestScalarsAreText", TestScalarsAreText),
        ("TestDeprecatedFields", TestDeprecatedFields),
        ("TestMetaTypesAreNotEmitted", TestMetaTypesAreNotEmitted),
        ("TestRootFunction", TestRootFunction),
        ("TestNoFormatArtifacts", TestNoFormatArtifacts),
        ("TestOutputIsDeterministic", TestOutputIsDeterministic),
    ];
    os::Exit(testing::Main(tests));
}
