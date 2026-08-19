//! Tests for the signature-to-table mapping.
//!
//! Plain `#[test]` functions, unlike every other suite in this repository: this
//! is the one crate that is not `no_std` on goish — it is a proc-macro crate
//! built for the host — so libtest is available and there is no `panic_impl` to
//! collide with.
//!
//! ```sh
//! cd sdk/macros && cargo test
//! ```
//!
//! What they can reach is the half of the crate that speaks in `String`.
//! Anything that builds or inspects a `TokenTree` cannot be tested here at all:
//! the `proc_macro` API panics with "procedural macro API is used outside of a
//! procedural macro" when called from a test binary. So the `Function` values
//! below are built by hand rather than parsed, and the options a parameter
//! carries — `default`, `default_path` — are out of reach, since those are
//! tokens. What is in reach is the type mapping, which is where a signature
//! turns into a `FunctionDef`.

use crate::parse::{
    variant_of, Enum, Function, ImplBlock, Param, SourceLoc, StructDef, StructField, Variant,
};
use crate::{
    camel_case, dispatch_arm, enum_defs, enum_impl, function_def, function_def_with,
    is_generator_return, kind_of, object_impl, source_map_def, state_impl, Enums, FunctionOptions,
};

/// A parameter with no `#[dagger(...)]` options.
///
/// Its source location is `unknown`: a `Span` is what carries one, and touching
/// the `proc_macro` API from a test binary panics.
fn param(name: &str, ty: &str) -> Param {
    Param {
        name: name.to_string(),
        ty: ty.to_string(),
        attrs: Vec::new(),
        source: SourceLoc::unknown(),
    }
}

/// The message `kind_of` refuses `ty` with.
///
/// Spelled out rather than reached for with `expect_err`, which wants a `Debug`
/// on the `Ok` type: `Kind` has none, and deriving one for a test is a
/// derive on the crate's own type for nobody's benefit.
fn refusal(ty: &str) -> String {
    match kind_of(ty, &Enums::default()) {
        Ok(_) => panic!("`{ty}` should have been refused"),
        Err(message) => message,
    }
}

/// A `#[dagger::function]` method taking `params` and returning `return_ty`.
fn function(name: &str, params: Vec<Param>, return_ty: &str) -> Function {
    Function {
        name: name.to_string(),
        doc: String::new(),
        source: SourceLoc::unknown(),
        params,
        return_ty: return_ty.to_string(),
        takes_self: true,
        markers: vec!["function".to_string()],
        options: Vec::new(),
    }
}

#[test]
fn scalars_map_to_their_kinds() {
    for (ty, kind, getter) in [
        ("string", "STRING_KIND", "string"),
        ("String", "STRING_KIND", "string"),
        ("int", "INTEGER_KIND", "int"),
        ("i64", "INTEGER_KIND", "int"),
        ("float64", "FLOAT_KIND", "float"),
        ("f64", "FLOAT_KIND", "float"),
        ("bool", "BOOLEAN_KIND", "bool"),
        ("", "VOID_KIND", "void"),
        ("()", "VOID_KIND", "void"),
    ] {
        let mapped = kind_of(ty, &Enums::default()).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert_eq!(mapped.kind, kind, "kind of `{ty}`");
        assert_eq!(mapped.getter, getter, "getter for `{ty}`");
        assert_eq!(mapped.type_name, "", "`{ty}` is not an object");
        assert!(!mapped.optional, "`{ty}` is not optional");
    }
}

/// Any type name that is not a scalar is an engine object, named to the engine
/// by the last segment of the path: how the user spells the import is theirs.
#[test]
fn object_types_are_named_by_their_last_segment() {
    for (ty, object) in [
        ("Directory", "Directory"),
        ("Container", "Container"),
        ("gen::Directory", "Directory"),
        ("dagger::gen::File", "File"),
        ("Workspace", "Workspace"),
        ("Changeset", "Changeset"),
    ] {
        let mapped = kind_of(ty, &Enums::default()).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert_eq!(mapped.kind, "OBJECT_KIND", "kind of `{ty}`");
        assert_eq!(mapped.type_name, object, "object name for `{ty}`");
        assert_eq!(mapped.getter, "object", "getter for `{ty}`");
    }
}

/// `Option<T>` is the only optionality marker, and it does not change what the
/// type underneath is.
#[test]
fn option_wraps_any_kind() {
    for (ty, kind, object) in [
        ("Option<string>", "STRING_KIND", ""),
        ("Option<int>", "INTEGER_KIND", ""),
        ("Option<f64>", "FLOAT_KIND", ""),
        ("Option<bool>", "BOOLEAN_KIND", ""),
        ("Option<Directory>", "OBJECT_KIND", "Directory"),
        ("Option<gen::Container>", "OBJECT_KIND", "Container"),
    ] {
        let mapped = kind_of(ty, &Enums::default()).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert!(mapped.optional, "`{ty}` is optional");
        assert_eq!(mapped.kind, kind, "kind of `{ty}`");
        assert_eq!(mapped.type_name, object, "object name for `{ty}`");
    }
}

/// A type that is not a plain name is not an object either, however much it
/// looks like one — there is no engine ID for a list or a reference, and
/// treating one as an object would register a type the engine has never heard
/// of instead of failing here.
#[test]
fn only_plain_type_names_are_objects() {
    for ty in ["& str", "& Directory", "[Directory]", "lowercase", "dyn Transport"] {
        assert!(
            kind_of(ty, &Enums::default()).is_err(),
            "`{ty}` should not be taken for an engine object"
        );
    }

    // A list *is* supported, but as a list of an object rather than as one:
    // `Vec<Directory>` must never register a type called `Vec<Directory>`.
    for ty in ["Vec<Directory>", "slice<Directory>"] {
        let mapped = kind_of(ty, &Enums::default()).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert!(mapped.list, "`{ty}` is a list");
        assert_eq!(mapped.type_name, "Directory", "object name for `{ty}`");
    }
}

/// A `slice<T>` or a `Vec<T>` is a list of `T`: the kind and the object name
/// stay the element's, and the list is one flag on top.
#[test]
fn lists_carry_their_element_kind() {
    for (ty, kind, object, getter) in [
        ("slice<string>", "STRING_KIND", "", "string_list"),
        ("Vec<string>", "STRING_KIND", "", "string_list"),
        ("goish::slice<string>", "STRING_KIND", "", "string_list"),
        ("slice<int>", "INTEGER_KIND", "", "int_list"),
        ("slice<f64>", "FLOAT_KIND", "", "float_list"),
        ("slice<float64>", "FLOAT_KIND", "", "float_list"),
        ("slice<bool>", "BOOLEAN_KIND", "", "bool_list"),
        ("slice<Directory>", "OBJECT_KIND", "Directory", "object_list"),
        ("Vec<gen::Container>", "OBJECT_KIND", "Container", "object_list"),
    ] {
        let mapped = kind_of(ty, &Enums::default()).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert!(mapped.list, "`{ty}` is a list");
        assert!(!mapped.optional, "`{ty}` is not optional");
        assert_eq!(mapped.kind, kind, "element kind of `{ty}`");
        assert_eq!(mapped.type_name, object, "object name for `{ty}`");
        assert_eq!(mapped.getter, getter, "getter for `{ty}`");
    }
}

/// An `Option` outside the list makes the *list* optional, which is what the
/// engine's `withOptional` on a `LIST_KIND` typedef means.
#[test]
fn an_option_around_a_list_makes_the_list_optional() {
    for (ty, kind, getter) in [
        ("Option<slice<string>>", "STRING_KIND", "string_list"),
        ("Option<Vec<int>>", "INTEGER_KIND", "int_list"),
        ("Option<slice<f64>>", "FLOAT_KIND", "float_list"),
        ("Option<slice<Directory>>", "OBJECT_KIND", "object_list"),
    ] {
        let mapped = kind_of(ty, &Enums::default()).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert!(mapped.list, "`{ty}` is a list");
        assert!(mapped.optional, "`{ty}` is optional");
        assert_eq!(mapped.kind, kind, "element kind of `{ty}`");
        assert_eq!(mapped.getter, getter, "getter for `{ty}`");
    }
}

/// The two list shapes that stop here, each refused by name rather than
/// registered as something a call would then fail on.
///
/// A list of lists is one level too many — nothing below `kind_of` carries it,
/// and the Dagger schema has none — and a list of optionals is a null element,
/// which has no Rust shape a dispatch could hand the function.
#[test]
fn deeper_list_shapes_are_refused() {
    let message = refusal("slice<slice<string>>");
    assert!(
        message.contains("a list goes one level deep"),
        "says how deep a list goes: {message}"
    );

    let message = refusal("slice<Option<string>>");
    assert!(
        message.contains("Option<slice<T>>"),
        "names what to write instead: {message}"
    );
}

/// An object argument is declared to the engine by name, and rebuilt from the
/// ID it arrives as before the function sees it.
#[test]
fn object_arguments_are_rebuilt_from_their_id() {
    let f = function("with_source", vec![param("source", "Directory")], "string");

    let def = function_def(&f, &Enums::default()).expect("a Directory argument is supported");
    assert!(
        def.contains(r#"kind: "OBJECT_KIND", type_name: "Directory", list: false, optional: false"#),
        "declared as an object: {def}"
    );

    let arm = dispatch_arm("Build", &f, &Enums::default()).expect("a Directory argument dispatches");
    assert!(
        arm.contains(r#"let source = <Directory as ::dagger::ObjectId>::from_id(args.object("source")?);"#),
        "rebuilt from its id: {arm}"
    );
}

/// An optional one is rebuilt only when it was supplied, and the type named is
/// the one inside the `Option`.
#[test]
fn optional_object_arguments_are_rebuilt_only_when_present() {
    let f = function(
        "with_source",
        vec![param("source", "Option<gen::Directory>")],
        "string",
    );

    let def = function_def(&f, &Enums::default()).expect("an optional Directory is supported");
    assert!(
        def.contains(r#"kind: "OBJECT_KIND", type_name: "Directory", list: false, optional: true"#),
        "declared optional: {def}"
    );

    let arm = dispatch_arm("Build", &f, &Enums::default()).expect("an optional Directory dispatches");
    assert!(
        arm.contains(
            r#"args.object_opt("source")?.map(<gen::Directory as ::dagger::ObjectId>::from_id)"#
        ),
        "rebuilt only when present: {arm}"
    );
}

/// A returned object is resolved to its ID, which is a round trip and so can
/// fail: the `?` is what carries that failure back to the engine.
#[test]
fn a_returned_object_is_encoded_as_its_id() {
    let f = function("base", Vec::new(), "Container");

    let def = function_def(&f, &Enums::default()).expect("a Container return is supported");
    assert!(
        def.contains(r#"return_kind: "OBJECT_KIND", return_type_name: "Container""#),
        "declared as an object return: {def}"
    );

    let arm = dispatch_arm("Build", &f, &Enums::default()).expect("a Container return dispatches");
    assert!(
        arm.contains("::dagger::__internal::encode_object(&self.base())?"),
        "encoded as its id, fallibly: {arm}"
    );
}

/// A list crosses the boundary as one value in each direction: the accessor
/// decodes the whole array, and the encoder writes it back.
#[test]
fn a_list_argument_and_return_are_declared_as_lists() {
    let f = function("tags", vec![param("names", "slice<string>")], "slice<string>");

    let def = function_def(&f, &Enums::default()).expect("a list of strings is supported");
    assert!(
        def.contains(r#"kind: "STRING_KIND", type_name: "", list: true, optional: false"#),
        "declared as a list of strings: {def}"
    );
    assert!(
        def.contains(r#"return_kind: "STRING_KIND", return_type_name: "", return_list: true"#),
        "declared as a list return: {def}"
    );

    let arm = dispatch_arm("Build", &f, &Enums::default()).expect("a list of strings dispatches");
    assert!(
        arm.contains(r#"let names = args.string_list("names")?;"#),
        "read as one list: {arm}"
    );
    assert!(
        arm.contains("::dagger::__internal::encode_string_list(&self.tags(names))"),
        "encoded as a list: {arm}"
    );
}

/// A list of floats reaches the float accessor and the float encoder, which are
/// a pair: the integer ones next door would round the fraction away.
#[test]
fn a_list_of_floats_uses_the_float_accessor_and_encoder() {
    let f = function("halved", vec![param("numbers", "slice<f64>")], "slice<f64>");

    let def = function_def(&f, &Enums::default()).expect("a list of floats is supported");
    assert!(
        def.contains(r#"kind: "FLOAT_KIND", type_name: "", list: true"#),
        "declared as a list of floats: {def}"
    );
    assert!(
        def.contains(r#"return_kind: "FLOAT_KIND", return_type_name: "", return_list: true"#),
        "declared as a float list return: {def}"
    );

    let arm = dispatch_arm("Build", &f, &Enums::default()).expect("a list of floats dispatches");
    assert!(
        arm.contains(r#"let numbers = args.float_list("numbers")?;"#),
        "read with the float list accessor: {arm}"
    );
    assert!(
        arm.contains("::dagger::__internal::encode_float_list(&self.halved(numbers))"),
        "encoded with the float list encoder: {arm}"
    );
}

/// A list of objects is a list of IDs on the wire, so it is rebuilt element by
/// element on the way in and resolved element by element on the way out.
#[test]
fn a_list_of_objects_goes_through_its_ids() {
    let f = function(
        "mount",
        vec![param("dirs", "slice<gen::Directory>")],
        "slice<Container>",
    );

    let def = function_def(&f, &Enums::default()).expect("a list of objects is supported");
    assert!(
        def.contains(r#"kind: "OBJECT_KIND", type_name: "Directory", list: true"#),
        "declared as a list of Directory: {def}"
    );

    let arm = dispatch_arm("Build", &f, &Enums::default()).expect("a list of objects dispatches");
    assert!(
        arm.contains(r#"let dirs = ::dagger::__internal::from_ids::<gen::Directory>(args.object_list("dirs")?);"#),
        "rebuilt from its ids: {arm}"
    );
    // Fallible for the reason a single object's is: each element's id is a
    // round trip.
    assert!(
        arm.contains("::dagger::__internal::encode_object_list(&self.mount(dirs))?"),
        "encoded as ids, fallibly: {arm}"
    );
}

/// An optional list is read through the `_opt` accessor, and a list of objects
/// is rebuilt only when one arrived — the same shape a single optional object
/// takes, with `from_ids` in place of `from_id`.
#[test]
fn an_optional_list_is_read_only_when_present() {
    let f = function(
        "tags",
        vec![
            param("names", "Option<slice<string>>"),
            param("dirs", "Option<Vec<Directory>>"),
        ],
        "string",
    );

    let def = function_def(&f, &Enums::default()).expect("an optional list is supported");
    assert!(
        def.contains(r#"kind: "STRING_KIND", type_name: "", list: true, optional: true"#),
        "the list is what is optional: {def}"
    );

    let arm = dispatch_arm("Build", &f, &Enums::default()).expect("an optional list dispatches");
    assert!(
        arm.contains(r#"let names = args.string_list_opt("names")?;"#),
        "read through the optional accessor: {arm}"
    );
    assert!(
        arm.contains(r#"args.object_list_opt("dirs")?.map(::dagger::__internal::from_ids::<Directory>)"#),
        "rebuilt only when present: {arm}"
    );
}

/// An `Option<T>` return is declared optional to the engine, and it is the
/// *return* typedef that carries it: the kind stays the one inside the `Option`,
/// so a caller sees a nullable String rather than some other type.
#[test]
fn an_optional_return_is_declared_optional() {
    for (ty, kind, name, list) in [
        ("Option<string>", "STRING_KIND", "", false),
        ("Option<int>", "INTEGER_KIND", "", false),
        ("Option<f64>", "FLOAT_KIND", "", false),
        ("Option<bool>", "BOOLEAN_KIND", "", false),
        ("Option<gen::Container>", "OBJECT_KIND", "Container", false),
        // An enum a module declared, which shares the field with an object's
        // name — the kind is what tells the two apart.
        ("Option<TargetOs>", "ENUM_KIND", "TargetOs", false),
        // Where this feature meets the list work: the `Option` goes around the
        // list, so both flags are set and the engine is told about a nullable
        // list rather than a list of nullable elements.
        ("Option<slice<string>>", "STRING_KIND", "", true),
        ("Option<Vec<gen::Directory>>", "OBJECT_KIND", "Directory", true),
    ] {
        let f = function("maybe", Vec::new(), ty);
        let def = function_def(&f, &declared(&["TargetOs"])).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert!(
            def.contains(&format!(
                r#"return_kind: "{kind}", return_type_name: "{name}", return_list: {list}, return_optional: true"#
            )),
            "`{ty}` declares an optional return: {def}"
        );
    }
}

/// A return that is not an `Option` says so, rather than leaving the field to be
/// read as whatever it defaults to: the engine applies `withOptional` from this
/// one flag, so a stale `true` would make every return nullable.
#[test]
fn a_plain_return_is_declared_required() {
    for ty in ["string", "int", "f64", "bool", "", "Container", "Result<string, string>"] {
        let f = function("plain", Vec::new(), ty);
        let def = function_def(&f, &Enums::default()).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert!(
            def.contains("return_optional: false"),
            "`{ty}` is not optional: {def}"
        );
    }
}

/// `None` is encoded as JSON null, and `Some` through the encoder for the kind
/// inside the `Option` — the same one a bare return of that kind uses.
///
/// The object arm is the one with a `?`: resolving a returned object's id is a
/// round trip. The `None` arm has no object to resolve, so it makes none.
#[test]
fn an_optional_return_encodes_none_as_null() {
    for (ty, some) in [
        ("Option<string>", "::dagger::__internal::encode_string(&__value)"),
        ("Option<int>", "::dagger::__internal::encode_int(__value)"),
        ("Option<f64>", "::dagger::__internal::encode_float(__value)"),
        ("Option<bool>", "::dagger::__internal::encode_bool(__value)"),
        ("Option<Container>", "::dagger::__internal::encode_object(&__value)?"),
        // A list encoder takes the same `__value`, so the two features compose
        // rather than compete: the `Some` arm encodes the whole list, and the
        // `None` arm is still the null. A guard on `ret.list` in front of the
        // optional split would have encoded a `slice` still inside its `Option`.
        ("Option<slice<string>>", "::dagger::__internal::encode_string_list(&__value)"),
        ("Option<slice<int>>", "::dagger::__internal::encode_int_list(&__value)"),
        ("Option<Vec<Directory>>", "::dagger::__internal::encode_object_list(&__value)?"),
        // An enum goes back as its member's name, which the value already
        // carries — so unlike the object arm this one has no `?`.
        ("Option<TargetOs>", "::dagger::__internal::encode_enum(&__value)"),
    ] {
        let f = function("maybe", Vec::new(), ty);
        let arm = dispatch_arm("Build", &f, &declared(&["TargetOs"]))
            .unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert!(
            arm.contains(&format!(
                "match self.maybe() {{ ::core::option::Option::Some(__value) => {some}, ::core::option::Option::None => ::dagger::__internal::encode_null() }}"
            )),
            "`{ty}` encodes both halves: {arm}"
        );
    }
}

/// A fallible function may return an `Option` too, and the two unwrappings
/// compose in one direction only: the `?` runs first, and what it yields is the
/// `Option` the match then reads.
#[test]
fn a_fallible_optional_return_composes_both() {
    let f = function("maybe", Vec::new(), "Result<Option<string>, string>");
    let arm =
        dispatch_arm("Build", &f, &Enums::default()).expect("a fallible optional return dispatches");
    assert!(
        arm.contains("match (self.maybe())? { ::core::option::Option::Some(__value) => ::dagger::__internal::encode_string(&__value), ::core::option::Option::None => ::dagger::__internal::encode_null() }"),
        "the `?` is inside the scrutinee: {arm}"
    );

    let def = function_def(&f, &Enums::default()).expect("a fallible optional return is declared");
    assert!(
        def.contains(
            r#"return_kind: "STRING_KIND", return_type_name: "", return_list: false, return_optional: true"#
        ),
        "declared by its ok type, and optional: {def}"
    );
}

/// A list of enum members is not supported, and wrapping it in an `Option` does
/// not smuggle it past.
///
/// The element-kind mapping has no `ENUM_KIND` entry, so `slice<MyEnum>` is
/// refused there — and `kind_of` unwraps the `Option` first, so the inner call
/// is what fails and `Option<slice<MyEnum>>` is refused with the same message.
/// The point is that it is refused *by name* rather than reaching the encoder,
/// which has no arm for a list of members and would emit code that does not
/// typecheck inside somebody's module.
#[test]
fn a_list_of_enum_members_is_refused_wrapped_or_not() {
    for ty in ["slice<TargetOs>", "Option<slice<TargetOs>>", "Option<Vec<TargetOs>>"] {
        let message = match kind_of(ty, &declared(&["TargetOs"])) {
            Err(message) => message,
            Ok(_) => panic!("`{ty}` should not be a supported type"),
        };
        assert!(
            message.contains("a list is of string, int, float, bool, or an engine object"),
            "`{ty}` says what a list may hold: {message}"
        );
    }
}

/// `Option<()>` says nothing a bare `()` does not: Void already is the absence of
/// a value, and it encodes as `null` either way, so the two would differ only in
/// a typedef the engine cannot tell apart.
#[test]
fn an_optional_void_is_refused() {
    for ty in ["Option<()>", "Result<Option<()>, string>"] {
        let f = function("nothing", Vec::new(), ty);
        let message = function_def(&f, &Enums::default()).expect_err("an optional Void is refused");
        assert!(
            message.contains("the absence of a value already"),
            "says why: {message}"
        );
    }
}

/// A generator's return is the changes it made. "Maybe some changes" and
/// "several sets of changes" are not shapes `dagger generate` can apply, so a
/// `Changeset` wrapped in either an `Option` or a `slice` stays refused now that
/// both wrappings are otherwise supported.
///
/// Asserted against the rule rather than against `function_def`, because being a
/// generator is carried by the tokens in `#[dagger::function(generate)]` and a
/// `TokenTree` cannot be built here at all — the `proc_macro` API panics outside
/// a macro expansion.
#[test]
fn a_generator_return_is_a_changeset_and_not_a_wrapped_one() {
    for (ty, ok) in [
        ("Changeset", true),
        ("gen::Changeset", true),
        ("Option<Changeset>", false),
        ("Option<gen::Changeset>", false),
        // A list of them is refused for the same reason: `dagger generate`
        // applies one set of changes, not several.
        ("slice<Changeset>", false),
        ("Option<slice<Changeset>>", false),
        ("Container", false),
        ("string", false),
    ] {
        let ret = kind_of(ty, &Enums::default()).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert_eq!(
            is_generator_return(&ret),
            ok,
            "`{ty}` as a generator's return"
        );
    }
}

/// A fallible function is declared by what it produces: the engine has no kind
/// for a failure, so the `Result` is peeled off before the type is mapped.
#[test]
fn a_fallible_return_is_declared_by_its_ok_type() {
    for (ty, kind, object) in [
        ("Result<string, string>", "STRING_KIND", ""),
        ("Result<int, string>", "INTEGER_KIND", ""),
        ("Result<f64, string>", "FLOAT_KIND", ""),
        ("Result<bool, string>", "BOOLEAN_KIND", ""),
        ("Result<(), string>", "VOID_KIND", ""),
        ("Result<gen::Container, string>", "OBJECT_KIND", "Container"),
        // The other error type: goish's `error`, which its own APIs fail with.
        ("Result<string, error>", "STRING_KIND", ""),
        ("Result<int, goish::error>", "INTEGER_KIND", ""),
        // Spelled the long way, as a module that imported neither would.
        ("core::result::Result<string, goish::gostring::string>", "STRING_KIND", ""),
    ] {
        let f = function("attempt", Vec::new(), ty);
        let def = function_def(&f, &Enums::default()).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert!(
            def.contains(&format!(r#"return_kind: "{kind}", return_type_name: "{object}""#)),
            "`{ty}` declares its ok type: {def}"
        );
    }
}

/// The failure itself needs no encoding: `invoke` returns the same
/// `Result<string, string>`, so `?` carries the message straight back to the
/// engine, which prints it the way `dagger::fail` does.
#[test]
fn a_fallible_return_is_passed_through_with_a_question_mark() {
    for (ty, encoded) in [
        ("Result<string, string>", "::dagger::__internal::encode_string(&(self.attempt())?)"),
        ("Result<int, string>", "::dagger::__internal::encode_int((self.attempt())?)"),
        ("Result<f64, string>", "::dagger::__internal::encode_float((self.attempt())?)"),
        ("Result<bool, string>", "::dagger::__internal::encode_bool((self.attempt())?)"),
        ("Result<(), string>", "{ (self.attempt())?; ::dagger::__internal::encode_void() }"),
        // Two `?`s, and they are different failures: the inner one is the
        // function's own, the outer is resolving the object it returned.
        ("Result<Container, string>", "::dagger::__internal::encode_object(&(self.attempt())?)?"),
        // A goish `error` is a value, not a message, so it is read through the
        // helper — `Error()` inline would panic on the nil one.
        (
            "Result<string, error>",
            "::dagger::__internal::encode_string(&(self.attempt()).map_err(::dagger::__internal::error_message)?)",
        ),
        (
            "Result<(), goish::error>",
            "{ (self.attempt()).map_err(::dagger::__internal::error_message)?; ::dagger::__internal::encode_void() }",
        ),
    ] {
        let f = function("attempt", Vec::new(), ty);
        let arm = dispatch_arm("Build", &f, &Enums::default()).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert!(arm.contains(encoded), "`{ty}` dispatches as `{encoded}`: {arm}");
    }
}

/// An error of any other type has nowhere to go — goish has no `Display`, and
/// its `error` is the interface a type carrying a message implements — so it is
/// refused by name rather than as a `From` error inside the macro's own output.
#[test]
fn a_non_string_error_is_refused() {
    for ty in ["Result<string, MyError>", "Result<string, ()>", "Result<string>"] {
        let f = function("attempt", Vec::new(), ty);
        let message = function_def(&f, &Enums::default()).expect_err("a non-string error is refused");
        assert!(
            message.contains("`Result<string, string>`"),
            "names what to write instead: {message}"
        );
    }
}

/// `#[dagger::function(deprecated = "...")]` becomes a reason on the function
/// itself, the same option a parameter carries one level down.
///
/// The reason is read off a `TokenTree`, which is out of reach here, so the
/// options are handed to the renderer directly — the seam `function_def_with`
/// exists for.
#[test]
fn a_deprecated_function_declares_its_reason() {
    let f = function("old_way", Vec::new(), "string");

    let options = FunctionOptions {
        generate: false,
        deprecated: "use newWay instead".to_string(),
    };
    let def = function_def_with(&f, &options, &Enums::default()).expect("a deprecated function is supported");
    assert!(
        def.contains(r#"deprecated: "use newWay instead""#),
        "carries the reason: {def}"
    );

    // Without the option the field is still emitted, and empty: `register`
    // reads "not deprecated" off the emptiness rather than off an Option.
    let plain = function_def(&f, &Enums::default()).expect("an ordinary function is supported");
    assert!(
        plain.contains(r#"deprecated: """#),
        "an undeprecated function declares no reason: {plain}"
    );
}

/// A source location becomes a `SourceMapDef`, and the absence of one becomes
/// `UNKNOWN` rather than a literal pointing at line zero.
#[test]
fn a_source_location_becomes_a_source_map() {
    let known = SourceLoc {
        file: "src/main.rs".to_string(),
        line: 42,
        column: 9,
    };
    assert_eq!(
        source_map_def(&known),
        r#"::dagger::__internal::SourceMapDef { file: "src/main.rs", line: 42, column: 9 }"#
    );
    assert_eq!(
        source_map_def(&SourceLoc::unknown()),
        "::dagger::__internal::SourceMapDef::UNKNOWN"
    );
}

/// A function and its arguments each declare where they were written.
#[test]
fn a_function_and_its_arguments_carry_their_source() {
    let mut f = function("build", vec![param("target", "string")], "string");
    f.source = SourceLoc {
        file: "src/main.rs".to_string(),
        line: 12,
        column: 5,
    };
    f.params[0].source = SourceLoc {
        file: "src/main.rs".to_string(),
        line: 13,
        column: 9,
    };

    let def = function_def(&f, &Enums::default()).expect("a located function is supported");
    assert!(
        def.contains(r#"source: ::dagger::__internal::SourceMapDef { file: "src/main.rs", line: 12, column: 5 }"#),
        "the function's own location: {def}"
    );
    assert!(
        def.contains(r#"source: ::dagger::__internal::SourceMapDef { file: "src/main.rs", line: 13, column: 9 }"#),
        "the argument's location: {def}"
    );
}

/// The `impl` block's doc comment becomes `Object::DOC`, which is both the
/// object's description and the module's.
#[test]
fn the_impl_doc_becomes_the_object_doc() {
    let block = ImplBlock {
        type_name: "Build".to_string(),
        doc: "Builds the project.".to_string(),
        source: SourceLoc {
            file: "src/main.rs".to_string(),
            line: 7,
            column: 6,
        },
        functions: Vec::new(),
    };

    let generated = object_impl(&block, &Enums::default()).expect("an empty impl block is supported");
    assert!(
        generated.contains(r#"const DOC: &'static str = "Builds the project.";"#),
        "the doc comment reaches the object: {generated}"
    );
    assert!(
        generated.contains(
            r#"const SOURCE: ::dagger::__internal::SourceMapDef = ::dagger::__internal::SourceMapDef { file: "src/main.rs", line: 7, column: 6 };"#
        ),
        "the type name's location reaches the object: {generated}"
    );
}

/// An undocumented block still declares a `DOC`, empty — `register` sends no
/// description rather than an empty one.
#[test]
fn an_undocumented_impl_declares_an_empty_doc() {
    let block = ImplBlock {
        type_name: "Build".to_string(),
        doc: String::new(),
        source: SourceLoc::unknown(),
        functions: Vec::new(),
    };

    let generated = object_impl(&block, &Enums::default()).expect("an undocumented impl block is supported");
    assert!(
        generated.contains(r#"const DOC: &'static str = "";"#),
        "no description: {generated}"
    );
    assert!(
        generated.contains("const SOURCE: ::dagger::__internal::SourceMapDef = ::dagger::__internal::SourceMapDef::UNKNOWN;"),
        "no source map: {generated}"
    );
}

/// A float crosses in both directions: the argument is read with the `float`
/// accessor, and the return goes back through the one encoder that is not
/// `encode_int`.
///
/// Both halves are asserted together because a getter or an encoder named one
/// letter off compiles here and fails inside somebody's module, which is the
/// same distance from the mistake that `__args` exists to keep.
#[test]
fn a_float_argument_and_return_use_the_float_accessor_and_encoder() {
    let f = function("scale", vec![param("factor", "f64")], "f64");

    let def = function_def(&f, &Enums::default()).expect("a float argument is supported");
    assert!(
        def.contains(r#"kind: "FLOAT_KIND", type_name: "", list: false, optional: false"#),
        "declared as a float: {def}"
    );
    assert!(
        def.contains(r#"return_kind: "FLOAT_KIND""#),
        "declared as a float return: {def}"
    );

    let arm = dispatch_arm("Build", &f, &Enums::default()).expect("a float argument dispatches");
    assert!(
        arm.contains(r#"let factor = args.float("factor")?;"#),
        "read with the float accessor: {arm}"
    );
    assert!(
        arm.contains("::dagger::__internal::encode_float(self.scale(factor))"),
        "encoded with the float encoder: {arm}"
    );
}

/// An `Option<f64>` reaches the optional accessor, the way every other optional
/// scalar reaches its own.
#[test]
fn an_optional_float_argument_uses_the_optional_accessor() {
    let f = function("scale", vec![param("factor", "Option<f64>")], "string");

    let def = function_def(&f, &Enums::default()).expect("an optional float is supported");
    assert!(
        def.contains(r#"kind: "FLOAT_KIND", type_name: "", list: false, optional: true"#),
        "declared optional: {def}"
    );

    let arm = dispatch_arm("Build", &f, &Enums::default()).expect("an optional float dispatches");
    assert!(
        arm.contains(r#"let factor = args.float_opt("factor")?;"#),
        "read with the optional float accessor: {arm}"
    );
}

/// `f32` is not a supported type, and is refused in the ordinary way.
///
/// There is no `f32` on the wire — GraphQL's Float is a double, and goish
/// models it as Go's `float64` — so accepting one would mean a lossy cast the
/// author never wrote. Declaring it a `FLOAT_KIND` anyway is worse than
/// declaring nothing, which is what it did before: the accessor hands back an
/// `f64` and the encoder takes an `f64`, so the mismatch surfaces as an E0308
/// pointing at the attribute, inside an expansion the author cannot read.
/// Refusing here turns that into a sentence.
#[test]
fn a_narrow_float_is_refused_like_any_other_unsupported_type() {
    let message = match kind_of("f32", &Enums::default()) {
        Err(message) => message,
        Ok(_) => panic!("`f32` should not be a supported type"),
    };
    assert!(
        message.contains("unsupported type `f32`"),
        "refused as an unsupported type: {message}"
    );
    // And by the same sentence every other unsupported type produces: nothing
    // float-specific, so what a reader learns is which scalars there are.
    assert!(
        message.contains("string, int, float, bool"),
        "with the ordinary message: {message}"
    );
}

/// A type with no kind is reported as unsupported, and the message lists the
/// scalars there *are*.
///
/// `f64` used to land here: it fell through to `is_object_name`, failed the
/// leading-uppercase test, and was reported as if it were a misspelled object.
/// Naming the scalars is what tells a reader which of the two it is.
#[test]
fn the_unsupported_type_message_lists_the_scalars() {
    // Matched rather than `expect_err`ed, unlike the refusals above: those come
    // back from functions whose Ok is a `String`, and `Kind` has no `Debug` for
    // `expect_err`'s bound to reach.
    let message = match kind_of("dyn Transport", &Enums::default()) {
        Err(message) => message,
        Ok(_) => panic!("`dyn Transport` is not a supported type"),
    };
    assert!(
        message.contains("string, int, float, bool"),
        "names the scalars a signature may use: {message}"
    );
}

// ─── enums ────────────────────────────────────────────────────────────

/// The enums a module declared, as `#[dagger::object(enums(...))]` listed them.
fn declared(paths: &[&str]) -> Enums {
    Enums { paths: paths.iter().map(|p| p.to_string()).collect() }
}

/// An enum, as `parse_enum` would have read it: the type's doc comment, and one
/// `(variant, doc)` pair per member.
fn declared_enum(name: &str, doc: &str, variants: &[(&str, &str)]) -> Enum {
    Enum {
        name: name.to_string(),
        doc: doc.to_string(),
        variants: variants
            .iter()
            .map(|(name, doc)| Variant { name: name.to_string(), doc: doc.to_string() })
            .collect(),
    }
}

/// A declared enum is an `ENUM_KIND`, not the object a type name would
/// otherwise be — which is the whole of what the `enums(...)` list changes.
#[test]
fn a_declared_enum_is_an_enum_kind() {
    let enums = declared(&["TargetOs"]);

    for ty in ["TargetOs", "crate::TargetOs"] {
        let mapped = kind_of(ty, &enums).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert_eq!(mapped.kind, "ENUM_KIND", "kind of `{ty}`");
        assert_eq!(mapped.type_name, "TargetOs", "enum name for `{ty}`");
        assert_eq!(mapped.getter, "enum_member", "getter for `{ty}`");
    }

    // The same name, undeclared, is an object: nothing about the two spellings
    // differs, so the list is the only thing that tells them apart.
    let mapped = kind_of("TargetOs", &Enums::default()).expect("a type name maps");
    assert_eq!(mapped.kind, "OBJECT_KIND", "an undeclared name is an object");
}

/// An enum argument arrives as a member name and is turned into the variant,
/// and a returned one goes back the same way.
#[test]
fn enum_arguments_and_returns_go_through_from_member() {
    let enums = declared(&["TargetOs"]);
    let f = function("build", vec![param("os", "TargetOs")], "TargetOs");

    let def = function_def(&f, &enums).expect("an enum argument is supported");
    assert!(
        def.contains(r#"kind: "ENUM_KIND", type_name: "TargetOs", list: false, optional: false"#),
        "declared as an enum: {def}"
    );
    assert!(
        def.contains(r#"return_kind: "ENUM_KIND", return_type_name: "TargetOs", return_list: false"#),
        "declared as an enum return: {def}"
    );

    let arm = dispatch_arm("Build", &f, &enums).expect("an enum argument dispatches");
    assert!(
        arm.contains(
            r#"let os = <TargetOs as ::dagger::__internal::EnumType>::from_member(&args.enum_member("os")?)?;"#
        ),
        "read as a member name: {arm}"
    );
    assert!(
        arm.contains("::dagger::__internal::encode_enum(&self.build(os))"),
        "returned as a member name: {arm}"
    );
}

/// An optional enum is read only when it was supplied — and not with `map`,
/// which would leave the `Result` that `from_member` returns inside the
/// `Option` the function is handed.
#[test]
fn an_optional_enum_argument_keeps_its_failure_outside_the_option() {
    let enums = declared(&["gen::TargetOs"]);
    let f = function("build", vec![param("os", "Option<TargetOs>")], "string");

    let def = function_def(&f, &enums).expect("an optional enum is supported");
    assert!(
        def.contains(r#"kind: "ENUM_KIND", type_name: "TargetOs", list: false, optional: true"#),
        "declared optional: {def}"
    );

    let arm = dispatch_arm("Build", &f, &enums).expect("an optional enum dispatches");
    assert!(
        arm.contains(r#"match args.enum_member_opt("os")?"#)
            && arm.contains("<TargetOs as ::dagger::__internal::EnumType>::from_member(&member)?"),
        "read only when present, and fallibly: {arm}"
    );
}

/// The enums a module declares are hung off the `Object` impl, since that is
/// the only thing `serve::<T>()` is handed — and only when there are any.
#[test]
fn declared_enums_reach_the_object_impl() {
    let enums = declared(&["TargetOs", "gen::Level"]);
    let listed = enum_defs(&enums);
    assert!(
        listed.contains("<TargetOs as ::dagger::__internal::EnumType>::DEF,")
            && listed.contains("<gen::Level as ::dagger::__internal::EnumType>::DEF,"),
        "each enum is named as the attribute spelled it: {listed}"
    );

    assert_eq!(
        enum_defs(&Enums::default()),
        "",
        "a module with no enums emits what it always did"
    );
}

/// The variant names are what crosses the boundary, in both directions, and
/// each doc comment is the description of the member it was written on.
#[test]
fn an_enum_declares_its_variants_and_their_docs() {
    let declared = declared_enum(
        "TargetOs",
        "The operating system a build targets.",
        &[("Alpine", "Alpine Linux."), ("DebianTesting", "")],
    );
    let emitted = enum_impl(&declared);

    assert!(
        emitted.contains(r#"name: "TargetOs","#)
            && emitted.contains(r#"doc: "The operating system a build targets.","#),
        "the type's own name and doc: {emitted}"
    );
    assert!(
        emitted
            .contains(r#"::dagger::__internal::EnumMemberDef { name: "Alpine", doc: "Alpine Linux." },"#),
        "a member carries its doc: {emitted}"
    );
    // Spelled as the variant is, not SCREAMING_SNAKE_CASE: the engine derives
    // the name a caller writes, and hands the module back this one.
    assert!(
        emitted.contains(r#"::dagger::__internal::EnumMemberDef { name: "DebianTesting", doc: "" },"#),
        "a member with no doc is still declared: {emitted}"
    );
    assert!(
        emitted.contains(r#"TargetOs::Alpine => "Alpine","#)
            && emitted.contains(r#"if name == "Alpine" {"#),
        "the name is written and read back: {emitted}"
    );
}

/// A variant that carries data has no engine equivalent — an enum there is a
/// set of member names — so it is refused by name rather than declared as if
/// the payload were not there.
#[test]
fn a_data_carrying_variant_is_refused() {
    for trailing in ["(string)", "{ x: int }", "= 1"] {
        let message = variant_of("TargetOs", "Tagged", trailing, String::new())
            .expect_err("a variant carrying data is refused");
        assert!(
            message.contains("`TargetOs::Tagged`") && message.contains("set of member names"),
            "names the variant and says why: {message}"
        );
    }

    let ok = variant_of("TargetOs", "Alpine", "", "Alpine Linux.".to_string())
        .expect("a bare variant is a member");
    assert_eq!(ok.name, "Alpine");
    assert_eq!(ok.doc, "Alpine Linux.");
}

/// The API spelling of a name is camelCase, whatever the Rust one is.
#[test]
fn names_are_camel_cased_for_the_api() {
    for (rust, api) in [
        ("container_echo", "containerEcho"),
        ("grep_dir", "grepDir"),
        ("build", "build"),
        ("with_gpu", "withGpu"),
    ] {
        assert_eq!(camel_case(rust), api, "camelCase of `{rust}`");
    }
}

// ─── state ────────────────────────────────────────────────────────────

/// A `pub` field with no `#[dagger(...)]` options.
fn field(name: &str, ty: &str) -> StructField {
    StructField {
        name: name.to_string(),
        ty: ty.to_string(),
        doc: String::new(),
        attrs: Vec::new(),
        is_pub: true,
        source: SourceLoc::unknown(),
    }
}

/// A struct with no fields and one with several, for the tests below.
fn struct_def(fields: Vec<StructField>) -> StructDef {
    StructDef {
        type_name: "Build".to_string(),
        fields,
    }
}

/// Every `pub` field becomes a `FieldDef`, camelCased and mapped through the
/// same table an argument is — a field and an argument of one type are the same
/// declaration seen from either side of the call.
#[test]
fn fields_become_field_defs() {
    let generated = state_impl(&struct_def(vec![
        field("image", "string"),
        field("jobs", "int"),
        field("with_gpu", "bool"),
        field("tag", "Option<string>"),
        field("source", "gen::Directory"),
        field("tags", "slice<string>"),
    ]), &Enums::default())
    .expect("a struct with fields is supported");

    for want in [
        r#"name: "image", kind: "STRING_KIND", type_name: "", list: false, optional: false"#,
        r#"name: "jobs", kind: "INTEGER_KIND", type_name: "", list: false, optional: false"#,
        r#"name: "withGpu", kind: "BOOLEAN_KIND", type_name: "", list: false, optional: false"#,
        r#"name: "tag", kind: "STRING_KIND", type_name: "", list: false, optional: true"#,
        r#"name: "source", kind: "OBJECT_KIND", type_name: "Directory", list: false, optional: false"#,
        r#"name: "tags", kind: "STRING_KIND", type_name: "", list: true, optional: false"#,
    ] {
        assert!(generated.contains(want), "declares {want}: {generated}");
    }
}

/// The read and the write of one field are a pair: the accessor that decodes it
/// out of the parent document and the encoder that puts it back have to agree,
/// or a value changes on the way through rather than failing.
#[test]
fn a_field_is_read_and_written_by_its_kind() {
    let generated = state_impl(&struct_def(vec![
        field("image", "string"),
        field("jobs", "int"),
        field("tag", "Option<string>"),
        field("source", "gen::Directory"),
        field("mounts", "slice<gen::Directory>"),
        field("tags", "slice<string>"),
    ]), &Enums::default())
    .expect("a struct with fields is supported");

    for want in [
        r#"image: state.string("image")?,"#,
        r#"jobs: state.int("jobs")?,"#,
        r#"tag: state.string_opt("tag")?,"#,
        r#"source: <gen::Directory as ::dagger::ObjectId>::from_id(state.object("source")?),"#,
        // A list of objects arrives as a list of ids and is rebuilt element by
        // element, exactly as a list-typed argument is.
        r#"mounts: ::dagger::__internal::from_ids::<gen::Directory>(state.object_list("mounts")?),"#,
        r#"tags: state.string_list("tags")?,"#,
        r#"__state.put("image", ::dagger::__internal::encode_string(&self.image));"#,
        r#"__state.put("jobs", ::dagger::__internal::encode_int(self.jobs));"#,
        r#"__state.put("source", ::dagger::__internal::encode_object(&self.source)?);"#,
        r#"__state.put("mounts", ::dagger::__internal::encode_object_list(&self.mounts)?);"#,
        r#"__state.put("tags", ::dagger::__internal::encode_string_list(&self.tags));"#,
    ] {
        assert!(generated.contains(want), "emits {want}: {generated}");
    }
    assert!(
        generated.contains("::dagger::__internal::encode_null()"),
        "an absent optional is written as null: {generated}"
    );
}

/// A unit struct still declares an `ObjectState`, with nothing in it: that is
/// what lets a module with no state be built from the empty document a
/// top-level call carries.
#[test]
fn a_unit_struct_declares_no_fields() {
    let generated = state_impl(&struct_def(Vec::new()), &Enums::default()).expect("a unit struct is supported");

    assert!(
        generated.contains("const FIELDS: &[::dagger::__internal::FieldDef] = &[];"),
        "an empty field table: {generated}"
    );
    assert!(
        generated.contains("::core::result::Result::Ok(Build {  })"),
        "rebuilt from nothing: {generated}"
    );
}

/// A private field would be state the engine never sees, and so state that does
/// not survive the call it was set in. Refused by name rather than dropped the
/// way Go drops an unexported field.
#[test]
fn a_private_field_is_refused() {
    let mut private = field("cache", "bool");
    private.is_pub = false;

    let message = state_impl(&struct_def(vec![private]), &Enums::default()).expect_err("a private field is refused");
    assert!(message.contains("Build.cache"), "names the field: {message}");
    assert!(message.contains("`pub`"), "says what to do: {message}");
}

/// A field of the object's own type cannot exist — the type would contain
/// itself — and letting it through would report that as an `ObjectId` bound
/// rather than as the field it is.
#[test]
fn a_field_of_the_objects_own_type_is_refused() {
    let message =
        state_impl(&struct_def(vec![field("parent", "Build")]), &Enums::default()).expect_err("refused by name");
    assert!(message.contains("own type"), "says why: {message}");
}

// ─── the constructor ──────────────────────────────────────────────────

/// A `#[dagger::constructor]` method taking `params` and returning `return_ty`.
fn constructor(params: Vec<Param>, return_ty: &str) -> Function {
    Function {
        name: "new".to_string(),
        doc: String::new(),
        source: SourceLoc::unknown(),
        params,
        return_ty: return_ty.to_string(),
        takes_self: false,
        markers: vec!["constructor".to_string()],
        options: Vec::new(),
    }
}

fn block(functions: Vec<Function>) -> ImplBlock {
    ImplBlock {
        type_name: "Build".to_string(),
        doc: String::new(),
        source: SourceLoc::unknown(),
        functions,
    }
}

/// The constructor is registered under the empty name, which is how the engine
/// spells "this function builds the object" — and it is not one of the object's
/// functions, or the module would serve one called `new` as well.
#[test]
fn a_constructor_is_registered_with_no_name() {
    let generated = object_impl(&block(vec![constructor(
        vec![param("image", "string")],
        "Build",
    )]), &Enums::default())
    .expect("a constructor is supported");

    assert!(
        generated
            .contains(r#"const CONSTRUCTOR: ::dagger::__internal::FunctionDef = ::dagger::__internal::FunctionDef { name: "","#),
        "registered with no name: {generated}"
    );
    assert!(
        generated.contains(r#"return_kind: "OBJECT_KIND", return_type_name: "Build""#),
        "returns the object: {generated}"
    );
    assert!(
        generated.contains("const FUNCTIONS: &[::dagger::__internal::FunctionDef] = &[];"),
        "and is not one of the functions: {generated}"
    );
    assert!(
        generated.contains(r#"let image = args.string("image")?;"#),
        "reads its arguments like any other function: {generated}"
    );
    assert!(
        generated.contains("::dagger::__internal::ObjectState::to_state(&Build::new(image))"),
        "encodes what it built: {generated}"
    );
}

/// A fallible constructor is declared by what it produces, the same rule a
/// function's return follows, and the failure is carried out before the state
/// is encoded.
#[test]
fn a_constructor_may_be_written_fallibly() {
    let generated = object_impl(&block(vec![constructor(
        Vec::new(),
        "Result<Build, string>",
    )]), &Enums::default())
    .expect("a fallible constructor is supported");

    assert!(
        generated.contains("::dagger::__internal::ObjectState::to_state(&(Build::new())?)"),
        "the `?` is inside the borrow: {generated}"
    );
}

/// A constructor that does not build the object is refused by name, rather than
/// as a type error somewhere inside the macro's own output.
#[test]
fn a_constructor_returning_something_else_is_refused() {
    for ty in ["string", "Container", "Option<Build>", ""] {
        let message = object_impl(&block(vec![constructor(Vec::new(), ty)]), &Enums::default())
            .expect_err("a constructor must return the object");
        assert!(
            message.contains("must return `Build`"),
            "says what it must return: {message}"
        );
    }
}

/// A constructor builds the object rather than being called on one, so a
/// receiver is a contradiction.
#[test]
fn a_constructor_taking_self_is_refused() {
    let mut f = constructor(Vec::new(), "Build");
    f.takes_self = true;

    let message = object_impl(&block(vec![f]), &Enums::default()).expect_err("a constructor takes no receiver");
    assert!(message.contains("self"), "names the receiver: {message}");
}

/// The engine registers exactly one, so a second is refused here rather than
/// silently replacing the first.
#[test]
fn a_second_constructor_is_refused() {
    let mut second = constructor(Vec::new(), "Build");
    second.name = "with_defaults".to_string();

    let message = object_impl(&block(vec![constructor(Vec::new(), "Build"), second]), &Enums::default())
        .expect_err("only one constructor is allowed");
    assert!(
        message.contains("second constructor"),
        "says what happened: {message}"
    );
}

/// The object's own type goes back as its state rather than as an id: the
/// engine holds no value to mint one for. This is the one place `encode_state`
/// and `encode_object` are told apart, and the name is the whole test.
#[test]
fn returning_the_objects_own_type_encodes_its_state() {
    let f = function("with_tag", vec![param("tag", "string")], "Build");

    let arm = dispatch_arm("Build", &f, &Enums::default()).expect("returning the object is supported");
    assert!(
        arm.contains("::dagger::__internal::encode_state(&self.with_tag(tag))?"),
        "encoded as its state: {arm}"
    );

    let other = function("base", Vec::new(), "Container");
    let arm = dispatch_arm("Build", &other, &Enums::default()).expect("returning an engine object is supported");
    assert!(
        arm.contains("::dagger::__internal::encode_object(&self.base())?"),
        "an engine object is still encoded as its id: {arm}"
    );
}
