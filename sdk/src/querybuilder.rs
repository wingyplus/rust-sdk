//! Multi-field selection: build a query, send it once, decode the result.
//!
//! A GraphQL round trip can ask for several fields at once, and Dang spells
//! that `user.{{ name, email }}`. This module is how Rust says the same thing:
//!
//! ```ignore
//! let (out, platform, (contents, size)) = ctr
//!     .with_exec(&["sh", "-c", "echo hi"])
//!     .fetch(|c| (
//!         c.stdout(),
//!         c.platform(),
//!         c.file("/etc/os-release").select(|f| (f.contents(), f.size())),
//!     ))?;
//! ```
//!
//! which renders as one document:
//!
//! ```graphql
//! {withExec(args:["sh","-c","echo hi"]){f0:stdout f1:platform f2:file(path:"/etc/os-release"){f0:contents f1:size}}}
//! ```
//!
//! **`fetch` sends, `select` builds.** Two verbs, so a stray top-level
//! `ctr.file("/x").select(…)?` fails with "`Sub<…>` is not a `Result`" rather
//! than something about a trait bound.
//!
//! # How it fits together
//!
//! [`Sel`] is the whole contract: render yourself into a query, then decode
//! yourself out of the response. Four shapes implement it, plus tuples:
//!
//! | type          | `Out`                 | emitted                     |
//! | ------------- | --------------------- | --------------------------- |
//! | [`Leaf<T>`]   | `T`                   | `fN:field`                  |
//! | [`Sub<S>`]    | `S::Out`              | `fN:field{…}`               |
//! | [`SubOpt<S>`] | `Option<S::Out>`      | same, nullable object field |
//! | [`SubList<S>`]| `slice<S::Out>`       | same, list-typed field      |
//! | `(A, B, …)`   | `(A::Out, B::Out, …)` | concatenation               |
//!
//! Fields are aliased positionally — `f0`, `f1`, … — which gets GraphQL
//! aliasing for free: the same field selected twice under different arguments
//! is just two leaves (`c.avatar_url(100), c.avatar_url(200)`).
//!
//! **The alias counter restarts inside every `{ }` and continues across
//! siblings.** That is the load-bearing detail, and render and decode walk the
//! selection in the same order so they agree on it without communicating.
//!
//! Everything here is monomorphised: no boxed decoders, no `dyn`, no arena, no
//! interior mutability. Nothing allocates beyond goish's `string` and `slice`.
//!
//! Sending is [`engine::fetch`](crate::engine::fetch).
//!
//! # Where this lives
//!
//! `sdk/src/`, never `sdk/src/gen/` — `dagger generate` replaces `src/gen/`
//! wholesale, the same rule that keeps [`Changeset`](crate::Changeset) and
//! [`Workspace`](crate::Workspace) at the crate root.
//!
//! # What codegen emits against it
//!
//! Per schema object `X`:
//!
//! ```ignore
//! pub struct X {                                  // the lazy chain, plus how to send it
//!     transport: Arc<dyn Transport>,
//!     q: Chain,
//! }
//! pub struct XFields;                             // zero-sized namespace
//! impl Fields for XFields { fn new() -> Self { XFields } }
//!
//! impl XFields {
//!     pub fn stdout(&self) -> Leaf<string> { Leaf::<string>::new("stdout") }
//!     pub fn file(&self, path: impl Into<string>) -> Field<FileFields> {
//!         let mut __args = Args::new();
//!         __args.put("path", arg_string(path));
//!         Field::<FileFields>::with_args("file", __args.finish())
//!     }
//! }
//!
//! impl X {
//!     pub fn fetch<S: Sel>(&self, f: impl FnOnce(&XFields) -> S) -> Result<S::Out, string> {
//!         engine::fetch(&*self.transport, &self.q, &f(&XFields::new()))
//!     }
//! }
//! ```
//!
//! The three builder types the plan calls `XField`, `XOptField` and
//! `XListField` are [`Field<F>`], [`OptField<F>`] and [`ListField<F>`] here —
//! one generic each rather than three emitted types per object. Completion is
//! unaffected: the closure parameter is still the concrete `&XFields`, which is
//! what rust-analyzer resolves against.
//!
//! Going the other way — a Rust value into GraphQL argument text — is
//! [`ToArg`], [`arg_string`] and [`arg_list`], accumulated by [`Args`]. Those
//! live here rather than in the generated code for the same reason the rest of
//! this module does: the generated half must be replaceable wholesale.

use core::marker::PhantomData;

use goish::encoding::json;
use goish::{append, float64, int, make, nil, slice, strconv, string};

/// One node of a selection.
///
/// Implementors render themselves into a query and decode themselves back out
/// of the response. `n` is the positional alias counter for the current
/// selection set: an implementor consumes exactly as many aliases in
/// [`decode`](Sel::decode) as it emits in [`render`](Sel::render), in the same
/// order, which is what keeps the two halves in step.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a selection",
    label = "not a selection",
    note = "a selection is a `Leaf`, a `Sub`/`SubOpt`/`SubList` from `.select(…)`, or a tuple of up to 16 of those — past 16, nest tuples"
)]
pub trait Sel {
    /// What decoding this selection produces.
    type Out;

    /// Append this selection's fields to `out`, consuming aliases from `n`.
    fn render(&self, out: &mut string, n: &mut usize);

    /// Read this selection's fields out of `value`, the object holding the
    /// aliases for the current selection set.
    fn decode(&self, value: &json::Value, n: &mut usize) -> Result<Self::Out, string>;
}

// ─── the alias counter ────────────────────────────────────────────────

/// The alias for position `n`.
fn alias(n: usize) -> string {
    string("f") + strconv::Itoa(n as int)
}

/// Emit `fN:field(args)` and advance the counter.
///
/// Siblings are separated by a space. A space is only needed between two bare
/// leaves — `f0:stdout f1:platform` would otherwise lex as one name — but
/// emitting it unconditionally keeps this one branch instead of three.
fn emit_head(out: &mut string, n: &mut usize, field: &'static str, args: &string) {
    if *n > 0 {
        *out += " ";
    }
    *out += alias(*n);
    *out += ":";
    *out += field;
    *out += args.clone();
    *n += 1;
}

/// Read the value at position `n` and advance the counter.
fn take(value: &json::Value, n: &mut usize) -> Result<json::Value, string> {
    let key = alias(*n);
    *n += 1;

    let object = match value.AsObject() {
        Some(o) => o,
        None => return Err(string("expected an object holding ") + key),
    };
    let (found, ok) = object.Get(key.clone());
    if !ok {
        return Err(string("the response is missing the selection aliased ") + key);
    }
    Ok(found)
}

// ─── Leaf ─────────────────────────────────────────────────────────────

/// A scalar, enum or list-of-scalar field: `fN:field`.
pub struct Leaf<T> {
    field: &'static str,
    args: string,
    // `fn() -> T` rather than `T` so a `Leaf<T>` is covariant in `T` and never
    // inherits an auto-trait bound from it — the value is produced by decoding,
    // never stored.
    out: PhantomData<fn() -> T>,
}

impl<T> Leaf<T> {
    /// A field taking no arguments.
    pub fn new(field: &'static str) -> Self {
        Leaf {
            field,
            args: string(""),
            out: PhantomData,
        }
    }

    /// A field with an already-rendered GraphQL argument list, parentheses
    /// included: `(path:"/etc/os-release")`.
    pub fn with_args(field: &'static str, args: string) -> Self {
        Leaf {
            field,
            args,
            out: PhantomData,
        }
    }
}

impl<T: FromJson> Sel for Leaf<T> {
    type Out = T;

    fn render(&self, out: &mut string, n: &mut usize) {
        emit_head(out, n, self.field, &self.args);
    }

    fn decode(&self, value: &json::Value, n: &mut usize) -> Result<T, string> {
        let found = take(value, n)?;
        T::from_json(&found).map_err(|why| string(self.field) + ": " + why)
    }
}

// ─── Sub, SubOpt, SubList ─────────────────────────────────────────────

/// A non-null object field with a nested selection: `fN:field{…}`.
pub struct Sub<S> {
    field: &'static str,
    args: string,
    inner: S,
}

impl<S> Sub<S> {
    /// Build one directly. Normally [`Field::select`] does this.
    pub fn new(field: &'static str, args: string, inner: S) -> Self {
        Sub { field, args, inner }
    }
}

impl<S: Sel> Sel for Sub<S> {
    type Out = S::Out;

    fn render(&self, out: &mut string, n: &mut usize) {
        render_sub(out, n, self.field, &self.args, &self.inner);
    }

    fn decode(&self, value: &json::Value, n: &mut usize) -> Result<S::Out, string> {
        let found = take(value, n)?;
        decode_sub(&found, self.field, &self.inner)
    }
}

/// A nullable object field. Decodes to `None` when the field is null.
///
/// The `Option` wraps the whole sub-record — `Option<(A, B)>`, not
/// `(Option<A>, Option<B>)` — matching Dang's rule that a selection on a null
/// receiver is null rather than an error. (Nullable *scalars* need no such
/// type: they are `Leaf<Option<T>>`.)
pub struct SubOpt<S> {
    field: &'static str,
    args: string,
    inner: S,
}

impl<S> SubOpt<S> {
    /// Build one directly. Normally [`OptField::select`] does this.
    pub fn new(field: &'static str, args: string, inner: S) -> Self {
        SubOpt { field, args, inner }
    }
}

impl<S: Sel> Sel for SubOpt<S> {
    type Out = Option<S::Out>;

    fn render(&self, out: &mut string, n: &mut usize) {
        render_sub(out, n, self.field, &self.args, &self.inner);
    }

    fn decode(&self, value: &json::Value, n: &mut usize) -> Result<Option<S::Out>, string> {
        let found = take(value, n)?;
        if found.IsNull() {
            return Ok(None);
        }
        Ok(Some(decode_sub(&found, self.field, &self.inner)?))
    }
}

/// A list-of-object field. Every element is decoded against the same
/// selection, each with its own alias counter.
pub struct SubList<S> {
    field: &'static str,
    args: string,
    inner: S,
}

impl<S> SubList<S> {
    /// Build one directly. Normally [`ListField::select`] does this.
    pub fn new(field: &'static str, args: string, inner: S) -> Self {
        SubList { field, args, inner }
    }
}

impl<S: Sel> Sel for SubList<S> {
    type Out = slice<S::Out>;

    fn render(&self, out: &mut string, n: &mut usize) {
        render_sub(out, n, self.field, &self.args, &self.inner);
    }

    fn decode(&self, value: &json::Value, n: &mut usize) -> Result<slice<S::Out>, string> {
        let found = take(value, n)?;
        let items = match found.AsArray() {
            Some(a) => a.clone(),
            None => return Err(string(self.field) + ": expected a list"),
        };

        let mut out = make!([]S::Out, 0, items.Len());
        let mut i: int = 0;
        while i < items.Len() {
            // Every element decodes from its own alias 0, and carries its index
            // in any failure — a list is the one place where naming the field
            // alone does not say which value went wrong.
            let mut nested: usize = 0;
            let decoded = self
                .inner
                .decode(&items[i], &mut nested)
                .map_err(|why| element_path(self.field, i) + ": " + why)?;
            out = append!(out, decoded);
            i += 1;
        }
        Ok(out)
    }
}

/// `field[i]` — where in a list a failure was found.
fn element_path(field: &'static str, i: int) -> string {
    string(field) + "[" + strconv::Itoa(i) + "]"
}

/// `fN:field(args){…}`, with the inner selection's counter starting fresh.
fn render_sub<S: Sel>(
    out: &mut string,
    n: &mut usize,
    field: &'static str,
    args: &string,
    inner: &S,
) {
    emit_head(out, n, field, args);
    *out += "{";
    let mut nested: usize = 0;
    inner.render(out, &mut nested);
    *out += "}";
}

/// Decode one selection set, from its own alias 0.
fn decode_sub<S: Sel>(value: &json::Value, field: &'static str, inner: &S) -> Result<S::Out, string> {
    let mut nested: usize = 0;
    inner
        .decode(value, &mut nested)
        .map_err(|why| string(field) + ": " + why)
}

// ─── tuples ───────────────────────────────────────────────────────────

macro_rules! tuple_sel {
    ($($name:ident),+) => {
        impl<$($name: Sel),+> Sel for ($($name,)+) {
            type Out = ($($name::Out,)+);

            #[allow(non_snake_case)]
            fn render(&self, out: &mut string, n: &mut usize) {
                let ($($name,)+) = self;
                $($name.render(out, n);)+
            }

            #[allow(non_snake_case)]
            fn decode(&self, value: &json::Value, n: &mut usize) -> Result<Self::Out, string> {
                let ($($name,)+) = self;
                // Tuple elements evaluate left to right, so the aliases are
                // consumed in the order render emitted them.
                Ok(($($name.decode(value, n)?,)+))
            }
        }
    };
}

// Sixteen, not twelve: `Container` has fields enough that a dozen is reachable
// in one selection, and past the cap you nest tuples — which the
// `on_unimplemented` note above says out loud, because the raw error is a
// trait-bound message that names no way out.
tuple_sel!(A);
tuple_sel!(A, B);
tuple_sel!(A, B, C);
tuple_sel!(A, B, C, D);
tuple_sel!(A, B, C, D, E);
tuple_sel!(A, B, C, D, E, F);
tuple_sel!(A, B, C, D, E, F, G);
tuple_sel!(A, B, C, D, E, F, G, H);
tuple_sel!(A, B, C, D, E, F, G, H, I);
tuple_sel!(A, B, C, D, E, F, G, H, I, J);
tuple_sel!(A, B, C, D, E, F, G, H, I, J, K);
tuple_sel!(A, B, C, D, E, F, G, H, I, J, K, L);
tuple_sel!(A, B, C, D, E, F, G, H, I, J, K, L, M);
tuple_sel!(A, B, C, D, E, F, G, H, I, J, K, L, M, N);
tuple_sel!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O);
tuple_sel!(A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P);

// ─── field namespaces and the builders that open them ─────────────────

/// The zero-sized namespace codegen emits per object — `ContainerFields` — one
/// method per schema field.
///
/// A selection closure receives `&F`, so completion at `|c| c.` offers exactly
/// the schema's field set and nothing else. It re-fires at every nesting depth
/// against the right object, because each closure's parameter type comes
/// straight off the `FnOnce` bound rather than from the enclosing selection.
pub trait Fields {
    /// The namespace value handed to a selection closure.
    fn new() -> Self;
}

/// An object-typed field, awaiting its nested selection.
pub struct Field<F> {
    field: &'static str,
    args: string,
    fields: PhantomData<fn() -> F>,
}

/// A nullable object-typed field, awaiting its nested selection.
pub struct OptField<F> {
    field: &'static str,
    args: string,
    fields: PhantomData<fn() -> F>,
}

/// A list-of-object field, awaiting the selection applied to every element.
pub struct ListField<F> {
    field: &'static str,
    args: string,
    fields: PhantomData<fn() -> F>,
}

macro_rules! builder {
    ($builder:ident, $node:ident, $what:literal) => {
        impl<F: Fields> $builder<F> {
            #[doc = concat!("A ", $what, " taking no arguments.")]
            pub fn new(field: &'static str) -> Self {
                $builder {
                    field,
                    args: string(""),
                    fields: PhantomData,
                }
            }

            #[doc = concat!("A ", $what, " with an already-rendered GraphQL argument list, parentheses included.")]
            pub fn with_args(field: &'static str, args: string) -> Self {
                $builder {
                    field,
                    args,
                    fields: PhantomData,
                }
            }

            /// Choose what to read from this field. Builds; does not send.
            pub fn select<S: Sel>(self, select: impl FnOnce(&F) -> S) -> $node<S> {
                $node::new(self.field, self.args, select(&F::new()))
            }
        }
    };
}

builder!(Field, Sub, "field");
builder!(OptField, SubOpt, "nullable field");
builder!(ListField, SubList, "list field");

// ─── scalars ──────────────────────────────────────────────────────────

/// How a leaf's JSON becomes a Rust value.
///
/// Implemented here for the scalar shapes the schema uses; codegen adds one
/// impl per generated enum.
pub trait FromJson: Sized {
    /// Decode one JSON value, or say what was expected.
    fn from_json(value: &json::Value) -> Result<Self, string>;
}

impl FromJson for string {
    fn from_json(value: &json::Value) -> Result<Self, string> {
        match value.AsString() {
            Some(s) => Ok(s.clone()),
            None => Err(string("expected a string")),
        }
    }
}

impl FromJson for bool {
    fn from_json(value: &json::Value) -> Result<Self, string> {
        match value.AsBool() {
            Some(b) => Ok(b),
            None => Err(string("expected a boolean")),
        }
    }
}

impl FromJson for int {
    fn from_json(value: &json::Value) -> Result<Self, string> {
        match value.AsNumber() {
            Some(n) => Ok(n as int),
            None => Err(string("expected a number")),
        }
    }
}

impl FromJson for float64 {
    fn from_json(value: &json::Value) -> Result<Self, string> {
        match value.AsNumber() {
            Some(n) => Ok(n),
            None => Err(string("expected a number")),
        }
    }
}

/// A nullable scalar. This is what a `Leaf<Option<T>>` decodes through.
impl<T: FromJson> FromJson for Option<T> {
    fn from_json(value: &json::Value) -> Result<Self, string> {
        if value.IsNull() {
            return Ok(None);
        }
        Ok(Some(T::from_json(value)?))
    }
}

/// A list of scalars. The schema has no nested lists — `[[T]]` never occurs —
/// so one level is the whole story, but this nests anyway if that changes.
impl<T: FromJson> FromJson for slice<T> {
    fn from_json(value: &json::Value) -> Result<Self, string> {
        let items = match value.AsArray() {
            Some(a) => a.clone(),
            None => return Err(string("expected a list")),
        };

        let mut out = make!([]T, 0, items.Len());
        let mut i: int = 0;
        while i < items.Len() {
            // The index, for the same reason [`SubList`] carries one: the
            // enclosing `Leaf` supplies the field name, and on its own that
            // does not say which element failed.
            let decoded =
                T::from_json(&items[i]).map_err(|why| string("[") + strconv::Itoa(i) + "]: " + why)?;
            out = append!(out, decoded);
            i += 1;
        }
        Ok(out)
    }
}

// ─── argument encoding ────────────────────────────────────────────────

/// Render a Rust-side string as a quoted, escaped literal.
///
/// Used for both the GraphQL arguments rendered here and the JSON request body
/// [`crate::engine`] wraps them in, which share escaping rules. Going through
/// `json::Marshal` rather than hand-rolling the escapes keeps module IDs —
/// opaque, engine-chosen text — safe to embed. It is public for the same reason
/// [`crate::engine::field`] is: a query written by hand has to quote its
/// arguments, and this is what the rest of the crate quotes with.
pub fn json_string(value: &string) -> string {
    let (encoded, err) = json::Marshal(&json::Value::String(value.clone()));
    if err != nil {
        crate::fail(string("encoding a query argument: ") + err.Error());
    }
    string(encoded)
}

/// How a Rust value becomes the text of one GraphQL argument.
///
/// The counterpart to [`FromJson`], for the direction that leaves: a leaf
/// decodes a response, a `ToArg` renders a request. Codegen emits one impl per
/// generated enum, ID scalar and input object, and calls [`arg_list`] for the
/// list-typed arguments; the shapes below are the ones the schema's built-in
/// scalars land on.
///
/// The rendered text is spliced straight into a query, so an implementor is
/// responsible for its own quoting — [`json_string`] for anything
/// string-shaped, nothing for a GraphQL enum literal, which is a bare name.
pub trait ToArg {
    /// This value as GraphQL argument text.
    fn to_arg(&self) -> string;
}

impl ToArg for string {
    fn to_arg(&self) -> string {
        json_string(self)
    }
}

impl ToArg for str {
    fn to_arg(&self) -> string {
        json_string(&string::from(self))
    }
}

impl ToArg for int {
    fn to_arg(&self) -> string {
        strconv::Itoa(*self)
    }
}

impl ToArg for float64 {
    fn to_arg(&self) -> string {
        // `-1` is Go's "shortest representation that round-trips", which is what
        // keeps a whole number out of exponent form.
        strconv::FormatFloat(*self, b'g', -1, 64)
    }
}

impl ToArg for bool {
    fn to_arg(&self) -> string {
        if *self {
            string("true")
        } else {
            string("false")
        }
    }
}

/// So a `&[&str]` list argument works without the caller owning its elements.
impl<T: ToArg + ?Sized> ToArg for &T {
    fn to_arg(&self) -> string {
        (**self).to_arg()
    }
}

/// A string-shaped argument, quoted and escaped.
///
/// Every string-backed scalar in the schema — `String`, `ID`, `Platform`,
/// `JSON`, and the per-object `ContainerID` family — renders the same way, so
/// codegen sends all of them through here. Taking `impl Into<string>` is what
/// lets a generated method accept a `&str` as readily as a `string`; the
/// [`ToArg`] impls cannot, since a bare `.into()` at the call site would have
/// nothing to infer its target from.
pub fn arg_string(value: impl Into<string>) -> string {
    json_string(&value.into())
}

/// An argument list under construction.
///
/// Codegen puts every argument of a field through one of these rather than
/// concatenating by hand, because whether a separator is needed depends on how
/// many optional arguments the caller actually supplied — which is not known
/// until run time.
pub struct Args {
    out: string,
    empty: bool,
}

impl Args {
    /// An empty argument list.
    pub fn new() -> Args {
        Args {
            out: string(""),
            empty: true,
        }
    }

    /// Add `name: value`, where `value` is already rendered — normally by
    /// [`ToArg::to_arg`] or [`arg_list`].
    pub fn put(&mut self, name: &'static str, value: string) {
        if !self.empty {
            self.out += ",";
        }
        self.out += name;
        self.out += ":";
        self.out += value;
        self.empty = false;
    }

    /// `(a:1,b:2)` — what a field's argument list looks like, or the empty
    /// string when no argument was supplied. A field with every argument
    /// omitted must render as a bare name, not as `()`.
    pub fn finish(self) -> string {
        if self.empty {
            return string("");
        }
        string("(") + self.out + ")"
    }

    /// `{a:1,b:2}` — the same list as an input-object literal.
    pub fn object(self) -> string {
        string("{") + self.out + "}"
    }
}

impl Default for Args {
    fn default() -> Args {
        Args::new()
    }
}

/// `[a,b,c]` — a list argument, each element rendered by its own [`ToArg`].
pub fn arg_list<T: ToArg>(items: &[T]) -> string {
    let mut out = string("[");
    let mut first = true;
    for item in items {
        if !first {
            out += ",";
        }
        out += item.to_arg();
        first = false;
    }
    out + "]"
}

// ─── the chain a selection hangs off ──────────────────────────────────

/// One step of a chain: a field and its rendered arguments.
#[derive(Clone)]
struct Step {
    name: &'static str,
    args: string,
    /// The type an inline fragment narrows this step to, or `""` for none.
    ///
    /// Needed by exactly one field, and it is the one every object goes
    /// through: `node(id:)` is typed as the `Node` interface, whose only field
    /// is `id`, so reading anything else off it takes `... on Container`.
    fragment: &'static str,
}

/// The lazily-built path from the query root to the object a selection is
/// taken on — `container{from(address:"alpine"){withExec(args:[…]){`.
///
/// Codegen's `X` holds one of these and appends to it in every chaining
/// method, then hands it to [`engine::fetch`](crate::engine::fetch) with
/// a selection.
///
/// Named `Chain` rather than `Query` on purpose: the schema's own root object
/// is called `Query`, so codegen must be free to emit `pub struct Query` for
/// it. (The Go SDK sidesteps the clash by calling that type `Client`.)
///
/// Chain steps are *not* aliased. Only the leaves of the final selection set
/// are, so a response nests under the plain field names, which is what
/// [`decode`](Chain::decode) walks.
#[derive(Clone)]
pub struct Chain {
    steps: slice<Step>,
}

impl Chain {
    /// The query root — an empty chain.
    pub fn root() -> Chain {
        Chain {
            steps: make!([]Step, 0, 4),
        }
    }

    /// Extend the chain by one field. Returns a new chain; the receiver is
    /// untouched, so a partially-built object can be reused for two calls.
    pub fn field(&self, name: &'static str, args: string) -> Chain {
        Chain {
            steps: append!(
                self.steps.clone(),
                Step {
                    name,
                    args,
                    fragment: "",
                }
            ),
        }
    }

    /// Extend the chain by one field, narrowed to `fragment` by an inline
    /// fragment: `name(args){... on Fragment{`.
    ///
    /// This exists for `node(id:)`, which is how an object is rebuilt from its
    /// id — the engine dropped the per-type `loadXFromID` loaders in favour of
    /// one Relay-style `node`, and `node` is typed as an interface. Without the
    /// fragment the server rejects every field but `id`.
    ///
    /// A fragment adds a brace to the *query* and no level to the *response*,
    /// which is why [`decode`](Chain::decode) does not mention it.
    pub fn field_on(&self, name: &'static str, args: string, fragment: &'static str) -> Chain {
        Chain {
            steps: append!(self.steps.clone(), Step {
                name,
                args,
                fragment
            }),
        }
    }

    /// The full query document for `sel` taken at the end of this chain.
    pub fn render<S: Sel>(&self, sel: &S) -> string {
        let mut out = string("{");
        let mut fragments: int = 0;
        let mut i: int = 0;
        while i < self.steps.Len() {
            out += self.steps[i].name;
            out += self.steps[i].args.clone();
            out += "{";
            if self.steps[i].fragment.len() > 0 {
                out += "... on ";
                out += self.steps[i].fragment;
                out += "{";
                fragments += 1;
            }
            i += 1;
        }

        let mut n: usize = 0;
        sel.render(&mut out, &mut n);

        // One closer per step and one per inline fragment, plus the document's
        // own brace.
        let mut closers = self.steps.Len() + fragments + 1;
        while closers > 0 {
            out += "}";
            closers -= 1;
        }
        out
    }

    /// Decode a response `data` object against `sel`, walking the chain first.
    pub fn decode<S: Sel>(&self, data: &json::Value, sel: &S) -> Result<S::Out, string> {
        let mut current = data.clone();
        let mut i: int = 0;
        while i < self.steps.Len() {
            let name = self.steps[i].name;
            let object = match current.AsObject() {
                Some(o) => o,
                None => return Err(string("expected an object at ") + name),
            };
            let (next, ok) = object.Get(name);
            if !ok {
                return Err(string("the response is missing ") + name);
            }
            current = next;
            i += 1;
        }

        let mut n: usize = 0;
        sel.decode(&current, &mut n)
    }
}
