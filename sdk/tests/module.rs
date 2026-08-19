//! Test suite for the module protocol's lists.
//!
//! ```sh
//! cd sdk && cargo test
//! ```
//!
//! A `harness = false` target, like `tests/querybuilder.rs` next to it and for
//! the same reason: libtest is `std` and its `panic_impl` collides with goish's,
//! so the tests are ordinary functions handed to `testing::Main` — the shape
//! `go test` generates — and cargo reads the exit status. **Add a test to the
//! list in `main` or it never runs.**
//!
//! What is here is the half of `module.rs` that has values to look at. The
//! registration side talks to an engine and the dispatch side is emitted by a
//! macro, but a list argument arrives as JSON text and a list result leaves as
//! JSON text, so both ends can be checked against a fixture: which shapes
//! decode, which are refused and with what message, and what the encoders
//! write.

#![no_std]
#![no_main]
#![allow(non_snake_case)]

use dagger::__internal::{
    encode_bool_list, encode_float_list, encode_int_list, encode_object_list, encode_string_list,
    from_ids, Arguments,
};
use dagger::{Changeset, ObjectId};
use goish::{append, float64, fmt, int, make, os, slice, string, testing};

/// The arguments of a call, as the engine hands them over: a name, and the
/// JSON *text* of the value.
fn arguments(entries: &[(&'static str, &'static str)]) -> Arguments {
    let mut out = make!([](string, string), 0, 0);
    for (name, value) in entries {
        out = append!(out, (string(*name), string(*value)));
    }
    Arguments::new(out)
}

fn assert_string(t: &testing::T, what: &'static str, got: string, want: &'static str) {
    if got != string(want) {
        t.Error(fmt::Sprintf!("%s = %q, want %q", what, got, want));
    }
}

/// A list of strings decodes element by element, in order.
fn TestAStringListDecodesEveryElement(t: &mut testing::T) {
    let args = arguments(&[("names", "[\"a\",\"b\",\"c\"]")]);

    let names = match args.string_list("names") {
        Ok(names) => names,
        Err(why) => t.Fatal(fmt::Sprintf!("string_list: %s", why)),
    };
    if names.Len() != 3 {
        t.Fatal(fmt::Sprintf!("names has %d elements, want 3", names.Len()));
    }
    assert_string(t, "names[0]", names[0].clone(), "a");
    assert_string(t, "names[1]", names[1].clone(), "b");
    assert_string(t, "names[2]", names[2].clone(), "c");
}

/// The other two element kinds, which differ only in how one value is read.
fn TestIntAndBoolListsDecode(t: &mut testing::T) {
    let args = arguments(&[("sizes", "[1,2,3]"), ("flags", "[true,false]")]);

    let sizes = match args.int_list("sizes") {
        Ok(sizes) => sizes,
        Err(why) => t.Fatal(fmt::Sprintf!("int_list: %s", why)),
    };
    if sizes.Len() != 3 {
        t.Fatal(fmt::Sprintf!("sizes has %d elements, want 3", sizes.Len()));
    }
    if sizes[0] != 1 || sizes[1] != 2 || sizes[2] != 3 {
        t.Error(fmt::Sprintf!("sizes = [%d %d %d], want [1 2 3]", sizes[0], sizes[1], sizes[2]));
    }

    let flags = match args.bool_list("flags") {
        Ok(flags) => flags,
        Err(why) => t.Fatal(fmt::Sprintf!("bool_list: %s", why)),
    };
    if flags.Len() != 2 {
        t.Fatal(fmt::Sprintf!("flags has %d elements, want 2", flags.Len()));
    }
    if !flags[0] || flags[1] {
        t.Error(fmt::Sprintf!("flags = [%t %t], want [true false]", flags[0], flags[1]));
    }
}

/// A list of floats is the one that must not narrow: the elements arrive as the
/// same JSON numbers an integer list reads, and the accessor either keeps the
/// fraction or silently rounds it away.
fn TestAFloatListKeepsItsFraction(t: &mut testing::T) {
    let args = arguments(&[("factors", "[1.5,2,-0.25]")]);

    let factors = match args.float_list("factors") {
        Ok(factors) => factors,
        Err(why) => t.Fatal(fmt::Sprintf!("float_list: %s", why)),
    };
    if factors.Len() != 3 {
        t.Fatal(fmt::Sprintf!("factors has %d elements, want 3", factors.Len()));
    }
    if factors[0] != 1.5 || factors[1] != 2.0 || factors[2] != -0.25 {
        t.Error(fmt::Sprintf!("factors[0] = %v, want 1.5", factors[0]));
    }

    // The integer accessor reading the same document is what shows the two are
    // different decoders rather than one with a cast on the end.
    match args.int_list("factors") {
        Ok(narrowed) => {
            if narrowed[0] != 1 {
                t.Error(fmt::Sprintf!("int_list narrowed 1.5 to %d, want 1", narrowed[0]));
            }
        }
        Err(why) => t.Error(fmt::Sprintf!("int_list: %s", why)),
    }
}

/// An empty list is a list. It is the one shape where "the caller passed
/// nothing" and "the caller passed no elements" could be confused, and a
/// required argument must accept the second.
fn TestAnEmptyListIsAListRatherThanAMissingArgument(t: &mut testing::T) {
    let args = arguments(&[("names", "[]")]);

    match args.string_list("names") {
        Ok(names) => {
            if names.Len() != 0 {
                t.Error(fmt::Sprintf!("names has %d elements, want 0", names.Len()));
            }
        }
        Err(why) => t.Error(fmt::Sprintf!("an empty list is a list: %s", why)),
    }

    match args.string_list_opt("names") {
        Ok(Some(names)) => {
            if names.Len() != 0 {
                t.Error(fmt::Sprintf!("names has %d elements, want 0", names.Len()));
            }
        }
        Ok(None) => t.Error("an empty list decoded as an absent one"),
        Err(why) => t.Error(fmt::Sprintf!("string_list_opt: %s", why)),
    }
}

/// An absent optional list is `None`, and so is one that arrived as JSON null —
/// which is how the engine sends an optional the caller left out. A *required*
/// list that never arrived is the missing-argument message.
fn TestAnAbsentListIsNoneAndAMissingRequiredArgument(t: &mut testing::T) {
    let args = arguments(&[("names", "null")]);

    match args.string_list_opt("names") {
        Ok(None) => {}
        Ok(Some(_)) => t.Error("a null list decoded to Some"),
        Err(why) => t.Error(fmt::Sprintf!("string_list_opt: %s", why)),
    }
    match args.string_list_opt("absent") {
        Ok(None) => {}
        Ok(Some(_)) => t.Error("an argument that was never sent decoded to Some"),
        Err(why) => t.Error(fmt::Sprintf!("string_list_opt: %s", why)),
    }

    match args.string_list("absent") {
        Ok(_) => t.Error("a missing required list decoded"),
        Err(why) => assert_string(t, "message", why, "missing required argument: absent"),
    }
}

/// A bad element names its index. The argument's own name fits every element of
/// the list equally well, which is no help to whoever has to find the value.
fn TestAListElementErrorNamesTheIndex(t: &mut testing::T) {
    let args = arguments(&[
        ("names", "[\"a\",3]"),
        ("sizes", "[1,\"two\"]"),
        ("flags", "[true,null]"),
        ("dirs", "[\"dir-1\",7]"),
    ]);

    match args.string_list("names") {
        Ok(_) => t.Error("a number decoded as a string"),
        Err(why) => assert_string(t, "message", why, "argument names[1] is not a string"),
    }
    match args.int_list("sizes") {
        Ok(_) => t.Error("a string decoded as an integer"),
        Err(why) => assert_string(t, "message", why, "argument sizes[1] is not an integer"),
    }
    match args.bool_list("flags") {
        Ok(_) => t.Error("a null decoded as a boolean"),
        Err(why) => assert_string(t, "message", why, "argument flags[1] is not a boolean"),
    }
    match args.object_list("dirs") {
        Ok(_) => t.Error("a number decoded as an object id"),
        Err(why) => assert_string(t, "message", why, "argument dirs[1] is not an object id"),
    }
}

/// A value that is not a list at all fails as the argument rather than as one of
/// its elements — there is no index to name.
fn TestAValueThatIsNotAListIsRefused(t: &mut testing::T) {
    let args = arguments(&[("names", "\"a\"")]);

    match args.string_list("names") {
        Ok(_) => t.Error("a bare string decoded as a list"),
        Err(why) => assert_string(t, "message", why, "argument names is not a list"),
    }
}

/// A list of objects arrives as a list of ids, and `from_ids` is what the
/// generated dispatch turns it into — the single-object rebuild, once per
/// element, in order.
fn TestAnObjectListArrivesAsIdsAndIsRebuilt(t: &mut testing::T) {
    let args = arguments(&[("changes", "[\"cs-1\",\"cs-2\"]")]);

    let ids = match args.object_list("changes") {
        Ok(ids) => ids,
        Err(why) => t.Fatal(fmt::Sprintf!("object_list: %s", why)),
    };
    assert_string(t, "changes[0]", ids[0].clone(), "cs-1");

    let rebuilt: slice<Changeset> = from_ids(ids);
    if rebuilt.Len() != 2 {
        t.Fatal(fmt::Sprintf!("rebuilt %d objects, want 2", rebuilt.Len()));
    }
    assert_string(t, "rebuilt[0]", rebuilt[0].id(), "cs-1");
    assert_string(t, "rebuilt[1]", rebuilt[1].id(), "cs-2");
}

/// A returned list is a JSON array of what each element encodes to, which for a
/// string means the quoting `encode_string` does — an element carrying a quote
/// of its own has to survive the round trip the engine decodes it with.
fn TestTheListEncodersWriteJsonArrays(t: &mut testing::T) {
    let mut names = make!([]string, 0, 2);
    names = append!(names, string("a"));
    names = append!(names, string("say \"hi\""));
    assert_string(
        t,
        "encode_string_list",
        encode_string_list(&names),
        "[\"a\",\"say \\\"hi\\\"\"]",
    );

    let mut sizes = make!([]int, 0, 2);
    sizes = append!(sizes, 1);
    sizes = append!(sizes, -2);
    assert_string(t, "encode_int_list", encode_int_list(&sizes), "[1,-2]");

    let mut flags = make!([]bool, 0, 2);
    flags = append!(flags, true);
    flags = append!(flags, false);
    assert_string(t, "encode_bool_list", encode_bool_list(&flags), "[true,false]");

    // `'g'` with precision -1, like `encode_float`: a whole number stays out of
    // exponent form and a fraction keeps its digits.
    let mut factors = make!([]float64, 0, 3);
    factors = append!(factors, 1.5);
    factors = append!(factors, 2.0);
    factors = append!(factors, -0.25);
    assert_string(t, "encode_float_list", encode_float_list(&factors), "[1.5,2,-0.25]");

    // The empty list, which is what a function that found nothing returns: `[]`
    // and not `null`, which is what a Void return encodes to.
    let empty = make!([]string, 0, 0);
    assert_string(t, "encode_string_list([])", encode_string_list(&empty), "[]");
}

/// A returned list of objects is a list of their ids, resolved one at a time.
///
/// `Changeset` is the object here because it is one of the two whose id is
/// already in hand — a generated object's would be a round trip, which is what
/// makes this encoder fallible in the first place.
fn TestAnObjectListIsEncodedAsItsIds(t: &mut testing::T) {
    let mut changes = make!([]Changeset, 0, 2);
    changes = append!(changes, Changeset::from_id(string("cs-1")));
    changes = append!(changes, Changeset::from_id(string("cs-2")));

    match encode_object_list(&changes) {
        Ok(encoded) => assert_string(t, "encode_object_list", encoded, "[\"cs-1\",\"cs-2\"]"),
        Err(why) => t.Error(fmt::Sprintf!("encode_object_list: %s", why)),
    }

    let empty = make!([]Changeset, 0, 0);
    match encode_object_list(&empty) {
        Ok(encoded) => assert_string(t, "encode_object_list([])", encoded, "[]"),
        Err(why) => t.Error(fmt::Sprintf!("encode_object_list: %s", why)),
    }
}

#[goish::main]
fn main() {
    let tests: &[(&str, testing::TestFn)] = &[
        (
            "TestAStringListDecodesEveryElement",
            TestAStringListDecodesEveryElement,
        ),
        ("TestIntAndBoolListsDecode", TestIntAndBoolListsDecode),
        (
            "TestAFloatListKeepsItsFraction",
            TestAFloatListKeepsItsFraction,
        ),
        (
            "TestAnEmptyListIsAListRatherThanAMissingArgument",
            TestAnEmptyListIsAListRatherThanAMissingArgument,
        ),
        (
            "TestAnAbsentListIsNoneAndAMissingRequiredArgument",
            TestAnAbsentListIsNoneAndAMissingRequiredArgument,
        ),
        (
            "TestAListElementErrorNamesTheIndex",
            TestAListElementErrorNamesTheIndex,
        ),
        (
            "TestAValueThatIsNotAListIsRefused",
            TestAValueThatIsNotAListIsRefused,
        ),
        (
            "TestAnObjectListArrivesAsIdsAndIsRebuilt",
            TestAnObjectListArrivesAsIdsAndIsRebuilt,
        ),
        (
            "TestTheListEncodersWriteJsonArrays",
            TestTheListEncodersWriteJsonArrays,
        ),
        (
            "TestAnObjectListIsEncodedAsItsIds",
            TestAnObjectListIsEncodedAsItsIds,
        ),
    ];
    os::Exit(testing::Main(tests));
}
