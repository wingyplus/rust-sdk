//! The engine objects a module can name today, and how one crosses the
//! boundary.
//!
//! An object is passed to a module, and returned from one, as its *ID*: an
//! opaque string the engine mints for a value it already holds. That is all an
//! object is here — there is no query builder yet, because that is what the
//! generated bindings will be. [`ObjectId`] is the seam: the generated types
//! will implement it too, so the dispatch `#[dagger::object]` emits keeps
//! working unchanged once they land.
//!
//! Only [`Changeset`] and [`Workspace`] are declared, and only because the
//! generator contract is written in terms of them — a `#[dagger::function(generate)]`
//! must return a `Changeset`, and the engine offers it a `Workspace` to read.
//! Every other object type is still a compile error naming the type.

use goish::string;

/// An engine object, named by the ID the engine minted for it.
///
/// Implemented by every type a module may take as an argument or hand back.
/// Which engine object a type stands for is `#[dagger::object]`'s to know — it
/// declares the name when it registers the module — so this carries only what
/// crossing the boundary needs.
pub trait ObjectId {
    /// Wrap an ID the engine supplied.
    fn from_id(id: string) -> Self;

    /// The ID this object was built from.
    fn id(&self) -> string;
}

/// A set of changes to a workspace's files.
///
/// What a generator returns: `dagger generate` runs the function and applies
/// the changeset to the workspace. Build one by asking the engine for it —
/// [`crate::Session::query`], until the bindings can do it for you:
///
/// ```ignore
/// let before = /* {loadWorkspaceFromID(id:…){directory(path:"/"){id}}} */;
/// let after = /* {loadDirectoryFromID(id:…){withNewFile(…){changes(from:…){id}}}} */;
/// Changeset::from_id(after)
/// ```
pub struct Changeset {
    id: string,
}

impl ObjectId for Changeset {
    fn from_id(id: string) -> Changeset {
        Changeset { id }
    }

    fn id(&self) -> string {
        self.id.clone()
    }
}

/// The workspace a call is running against.
///
/// The engine injects it: a function that declares a `Workspace` argument is
/// still callable with no arguments, which is what lets a generator take one.
pub struct Workspace {
    id: string,
}

impl ObjectId for Workspace {
    fn from_id(id: string) -> Workspace {
        Workspace { id }
    }

    fn id(&self) -> string {
        self.id.clone()
    }
}
