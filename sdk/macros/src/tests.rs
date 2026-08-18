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

use crate::parse::{Function, ImplBlock, Param, SourceLoc};
use crate::{
    camel_case, dispatch_arm, function_def, function_def_with, kind_of, object_impl,
    source_map_def, FunctionOptions,
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
        let mapped = kind_of(ty).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert_eq!(mapped.kind, kind, "kind of `{ty}`");
        assert_eq!(mapped.getter, getter, "getter for `{ty}`");
        assert_eq!(mapped.object, "", "`{ty}` is not an object");
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
        let mapped = kind_of(ty).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert_eq!(mapped.kind, "OBJECT_KIND", "kind of `{ty}`");
        assert_eq!(mapped.object, object, "object name for `{ty}`");
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
        let mapped = kind_of(ty).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert!(mapped.optional, "`{ty}` is optional");
        assert_eq!(mapped.kind, kind, "kind of `{ty}`");
        assert_eq!(mapped.object, object, "object name for `{ty}`");
    }
}

/// A type that is not a plain name is not an object either, however much it
/// looks like one — there is no engine ID for a list or a reference, and
/// treating one as an object would register a type the engine has never heard
/// of instead of failing here.
#[test]
fn only_plain_type_names_are_objects() {
    for ty in [
        "Vec<string>",
        "slice<Directory>",
        "& str",
        "& Directory",
        "[Directory]",
        "lowercase",
        "dyn Transport",
    ] {
        assert!(
            kind_of(ty).is_err(),
            "`{ty}` should not be taken for an engine object"
        );
    }
}

/// An object argument is declared to the engine by name, and rebuilt from the
/// ID it arrives as before the function sees it.
#[test]
fn object_arguments_are_rebuilt_from_their_id() {
    let f = function("with_source", vec![param("source", "Directory")], "string");

    let def = function_def(&f).expect("a Directory argument is supported");
    assert!(
        def.contains(r#"kind: "OBJECT_KIND", object: "Directory", optional: false"#),
        "declared as an object: {def}"
    );

    let arm = dispatch_arm("Build", &f).expect("a Directory argument dispatches");
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

    let def = function_def(&f).expect("an optional Directory is supported");
    assert!(
        def.contains(r#"kind: "OBJECT_KIND", object: "Directory", optional: true"#),
        "declared optional: {def}"
    );

    let arm = dispatch_arm("Build", &f).expect("an optional Directory dispatches");
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

    let def = function_def(&f).expect("a Container return is supported");
    assert!(
        def.contains(r#"return_kind: "OBJECT_KIND", return_object: "Container""#),
        "declared as an object return: {def}"
    );

    let arm = dispatch_arm("Build", &f).expect("a Container return dispatches");
    assert!(
        arm.contains("::dagger::encode_object(&Build.base())?"),
        "encoded as its id, fallibly: {arm}"
    );
}

/// An optional return has nowhere to go — `FunctionDef` cannot declare one, and
/// the encoders take the bare value — so it is refused by name rather than as a
/// type error inside the macro's own output.
#[test]
fn an_optional_return_is_refused() {
    for ty in ["Option<string>", "Option<Container>"] {
        let f = function("maybe", Vec::new(), ty);
        let message = dispatch_arm("Build", &f).expect_err("an optional return is refused");
        assert!(
            message.contains("optional return is not supported"),
            "says why: {message}"
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
        let def = function_def(&f).unwrap_or_else(|e| panic!("{ty}: {e}"));
        assert!(
            def.contains(&format!(r#"return_kind: "{kind}", return_object: "{object}""#)),
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
        ("Result<string, string>", "::dagger::encode_string(&(Build.attempt())?)"),
        ("Result<int, string>", "::dagger::encode_int((Build.attempt())?)"),
        ("Result<f64, string>", "::dagger::encode_float((Build.attempt())?)"),
        ("Result<bool, string>", "::dagger::encode_bool((Build.attempt())?)"),
        ("Result<(), string>", "{ (Build.attempt())?; ::dagger::encode_void() }"),
        // Two `?`s, and they are different failures: the inner one is the
        // function's own, the outer is resolving the object it returned.
        ("Result<Container, string>", "::dagger::encode_object(&(Build.attempt())?)?"),
        // A goish `error` is a value, not a message, so it is read through the
        // helper — `Error()` inline would panic on the nil one.
        (
            "Result<string, error>",
            "::dagger::encode_string(&(Build.attempt()).map_err(::dagger::error_message)?)",
        ),
        (
            "Result<(), goish::error>",
            "{ (Build.attempt()).map_err(::dagger::error_message)?; ::dagger::encode_void() }",
        ),
    ] {
        let f = function("attempt", Vec::new(), ty);
        let arm = dispatch_arm("Build", &f).unwrap_or_else(|e| panic!("{ty}: {e}"));
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
        let message = function_def(&f).expect_err("a non-string error is refused");
        assert!(
            message.contains("`Result<string, string>`"),
            "names what to write instead: {message}"
        );
    }
}

/// The rules a return is held to are the ok type's, fallible or not.
#[test]
fn a_fallible_optional_return_is_refused_too() {
    let f = function("maybe", Vec::new(), "Result<Option<string>, string>");
    let message = dispatch_arm("Build", &f).expect_err("an optional return is refused");
    assert!(
        message.contains("optional return is not supported") && message.contains("return `string`"),
        "says why, and what to return instead: {message}"
    );
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
    let def = function_def_with(&f, &options).expect("a deprecated function is supported");
    assert!(
        def.contains(r#"deprecated: "use newWay instead""#),
        "carries the reason: {def}"
    );

    // Without the option the field is still emitted, and empty: `register`
    // reads "not deprecated" off the emptiness rather than off an Option.
    let plain = function_def(&f).expect("an ordinary function is supported");
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
        r#"::dagger::SourceMapDef { file: "src/main.rs", line: 42, column: 9 }"#
    );
    assert_eq!(
        source_map_def(&SourceLoc::unknown()),
        "::dagger::SourceMapDef::UNKNOWN"
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

    let def = function_def(&f).expect("a located function is supported");
    assert!(
        def.contains(r#"source: ::dagger::SourceMapDef { file: "src/main.rs", line: 12, column: 5 }"#),
        "the function's own location: {def}"
    );
    assert!(
        def.contains(r#"source: ::dagger::SourceMapDef { file: "src/main.rs", line: 13, column: 9 }"#),
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

    let generated = object_impl(&block).expect("an empty impl block is supported");
    assert!(
        generated.contains(r#"const DOC: &'static str = "Builds the project.";"#),
        "the doc comment reaches the object: {generated}"
    );
    assert!(
        generated.contains(
            r#"const SOURCE: ::dagger::SourceMapDef = ::dagger::SourceMapDef { file: "src/main.rs", line: 7, column: 6 };"#
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

    let generated = object_impl(&block).expect("an undocumented impl block is supported");
    assert!(
        generated.contains(r#"const DOC: &'static str = "";"#),
        "no description: {generated}"
    );
    assert!(
        generated.contains("const SOURCE: ::dagger::SourceMapDef = ::dagger::SourceMapDef::UNKNOWN;"),
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

    let def = function_def(&f).expect("a float argument is supported");
    assert!(
        def.contains(r#"kind: "FLOAT_KIND", object: "", optional: false"#),
        "declared as a float: {def}"
    );
    assert!(
        def.contains(r#"return_kind: "FLOAT_KIND""#),
        "declared as a float return: {def}"
    );

    let arm = dispatch_arm("Build", &f).expect("a float argument dispatches");
    assert!(
        arm.contains(r#"let factor = args.float("factor")?;"#),
        "read with the float accessor: {arm}"
    );
    assert!(
        arm.contains("::dagger::encode_float(Build.scale(factor))"),
        "encoded with the float encoder: {arm}"
    );
}

/// An `Option<f64>` reaches the optional accessor, the way every other optional
/// scalar reaches its own.
#[test]
fn an_optional_float_argument_uses_the_optional_accessor() {
    let f = function("scale", vec![param("factor", "Option<f64>")], "string");

    let def = function_def(&f).expect("an optional float is supported");
    assert!(
        def.contains(r#"kind: "FLOAT_KIND", object: "", optional: true"#),
        "declared optional: {def}"
    );

    let arm = dispatch_arm("Build", &f).expect("an optional float dispatches");
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
    let message = match kind_of("f32") {
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
    let message = match kind_of("dyn Transport") {
        Err(message) => message,
        Ok(_) => panic!("`dyn Transport` is not a supported type"),
    };
    assert!(
        message.contains("string, int, float, bool"),
        "names the scalars a signature may use: {message}"
    );
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
