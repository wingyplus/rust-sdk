//! How an engine object crosses the call boundary, and the two objects that
//! are declared by hand rather than generated.
//!
//! An object is passed to a module, and returned from one, as its *ID*: an
//! opaque string the engine mints for a value it already holds. [`ObjectId`] is
//! that seam, and the generated bindings in [`crate::gen`] implement it too — a
//! `Directory` argument arrives as an ID, is rebuilt into a real client object
//! through [`ObjectId::from_id`], and every method on it works from there.
//!
//! [`Changeset`] and [`Workspace`] stay here, hand-written, because the
//! generator contract is written in terms of them and `src/gen/` is a
//! placeholder until the module's first `dagger generate` — a scaffolded module
//! has to compile before it can generate. They are ID wrappers with no client
//! behind them; the generated types of the same name are the full objects.

use goish::string;

/// An engine object, named by the ID the engine minted for it.
///
/// Implemented by every type a module may take as an argument or hand back:
/// the two wrappers below, and every generated object the engine has a
/// `loadXFromID` for. Which engine object a type stands for is
/// `#[dagger::object]`'s to know — it declares the name when it registers the
/// module — so this carries only what crossing the boundary needs.
///
/// # Where the connection comes from
///
/// [`from_id`](ObjectId::from_id) takes only the ID, not a transport, because a
/// generated object opens the session the engine left in this process's
/// environment — the same thing `dag()` does, for the same reason. So the
/// dispatch `#[dagger::object]` emits has nothing to thread through, and a
/// caller rebuilding an object by hand needs nothing either.
///
/// # Why the note about enums
///
/// A signature naming a plain type is registered as an engine object of that
/// name — the macro has no schema to check it against — and the check that it
/// *is* one is this trait. So an enum the module itself declares lands here
/// too, unless it was named in `#[dagger::object(enums(...))]`, and the error
/// says so rather than leaving a `Directory` typo and a forgotten enum looking
/// identical.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not an engine object",
    label = "not an engine object",
    note = "if `{Self}` is an enum this module declares, mark it `#[dagger::enum_type]` and name it in `#[dagger::object(enums({Self}))]`; otherwise the engine has no loader for an object of that name, so check the spelling against the schema"
)]
pub trait ObjectId: Sized {
    /// Rebuild an object from an ID the engine supplied.
    fn from_id(id: string) -> Self;

    /// The ID for this object.
    ///
    /// Fallible because a generated object is a *chain* — `container().from(…)
    /// .with_exec(…)` — that nothing has sent yet, so asking for its ID runs
    /// it. For the wrappers below the ID is already in hand and this cannot
    /// fail.
    fn to_id(&self) -> Result<string, string>;
}

/// A set of changes to a workspace's files.
///
/// What a generator returns: `dagger generate` runs the function and applies
/// the changeset to the workspace. Build one by asking the engine for it —
/// [`crate::engine::Session::query`], until the bindings can do it for you:
///
/// ```ignore
/// let before = /* {loadWorkspaceFromID(id:…){directory(path:"/"){id}}} */;
/// let after = /* {loadDirectoryFromID(id:…){withNewFile(…){changes(from:…){id}}}} */;
/// Changeset::from_id(after)
/// ```
pub struct Changeset {
    id: string,
}

impl Changeset {
    /// The ID this changeset was built from.
    ///
    /// Inherent and infallible, unlike [`ObjectId::to_id`]: a wrapper is
    /// nothing but the ID, so there is no query to run.
    pub fn id(&self) -> string {
        self.id.clone()
    }
}

impl ObjectId for Changeset {
    fn from_id(id: string) -> Changeset {
        Changeset { id }
    }

    fn to_id(&self) -> Result<string, string> {
        Ok(self.id.clone())
    }
}

/// The workspace a call is running against.
///
/// The engine injects it: a function that declares a `Workspace` argument is
/// still callable with no arguments, which is what lets a generator take one.
pub struct Workspace {
    id: string,
}

impl Workspace {
    /// The ID the engine handed this workspace over as.
    ///
    /// Inherent and infallible, unlike [`ObjectId::to_id`]; see
    /// [`Changeset::id`].
    pub fn id(&self) -> string {
        self.id.clone()
    }
}

impl ObjectId for Workspace {
    fn from_id(id: string) -> Workspace {
        Workspace { id }
    }

    fn to_id(&self) -> Result<string, string> {
        Ok(self.id.clone())
    }
}
