//! The introspection schema, decoded into what the renderer needs.
//!
//! GraphQL introspection nests a type reference as a chain of wrappers —
//! `NON_NULL(LIST(NON_NULL(String)))` — and the renderer never wants the chain,
//! only the three things it means: which named type, whether it is a list, and
//! whether it can be null. [`TypeRef`] is that flattening, which is also why
//! nothing here is recursive and no `Box` appears.
//!
//! The flattening is lossy in exactly one way: the nullability of a list's
//! *elements* is dropped. Every list in the engine's schema is a list of
//! non-null elements, in both field and argument position, so there is nothing
//! to keep.
//!
//! Decoding is deliberately forgiving of missing keys — an absent `description`
//! is an empty one, absent `fields` an empty list — because introspection omits
//! rather than nulls, and a generator that stopped on every omission would stop
//! on the first scalar it read.

use goish::encoding::json;
use goish::{append, bytes, error, errors, make, slice, string};

/// A reference to a named type, with its wrappers flattened away.
#[derive(Clone)]
pub struct TypeRef {
    /// The innermost named type: `String`, `Container`, `CacheSharingMode`.
    pub name: string,
    /// That type's kind: `SCALAR`, `OBJECT`, `INTERFACE`, `ENUM`,
    /// `INPUT_OBJECT`.
    pub kind: string,
    /// Whether a `LIST` wrapper appeared anywhere in the chain.
    pub list: bool,
    /// Whether the outermost wrapper was `NON_NULL`.
    pub non_null: bool,
}

/// One argument of a field, or one field of an input object — introspection
/// describes both with `__InputValue`.
#[derive(Clone)]
pub struct InputValue {
    pub name: string,
    pub doc: string,
    pub ty: TypeRef,
    /// The GraphQL-encoded default, or empty when there is none. Only ever read
    /// for documentation: an argument is optional because it is nullable, and
    /// every defaulted argument in the schema is nullable.
    pub default_value: string,
}

/// One field of an object or interface type.
#[derive(Clone)]
pub struct Field {
    pub name: string,
    pub doc: string,
    pub ty: TypeRef,
    pub args: slice<InputValue>,
    /// The deprecation reason, or empty when the field is current.
    pub deprecated: string,
}

/// One value of an enum type.
#[derive(Clone)]
pub struct EnumValue {
    pub name: string,
    pub doc: string,
}

/// One type in the schema.
#[derive(Clone)]
pub struct Type {
    /// `OBJECT`, `INTERFACE`, `SCALAR`, `ENUM`, `INPUT_OBJECT`, `UNION`.
    pub kind: string,
    pub name: string,
    pub doc: string,
    /// Populated for `OBJECT` and `INTERFACE`.
    pub fields: slice<Field>,
    /// Populated for `INPUT_OBJECT`.
    pub input_fields: slice<InputValue>,
    /// Populated for `ENUM`.
    pub enum_values: slice<EnumValue>,
}

impl Type {
    /// The field of this type named `name`, if it has one.
    pub fn field<S: Into<string>>(&self, name: S) -> Option<Field> {
        let name = name.into();
        let mut i: goish::int = 0;
        while i < self.fields.Len() {
            if self.fields[i].name == name {
                return Some(self.fields[i].clone());
            }
            i += 1;
        }
        None
    }
}

/// The whole schema, with its types sorted by name.
pub struct Schema {
    pub types: slice<Type>,
    /// The name of the root query type — `Query`.
    pub query_type: string,
}

impl Schema {
    /// The type named `name`, if the schema has one.
    pub fn find(&self, name: &string) -> Option<Type> {
        let mut i: goish::int = 0;
        while i < self.types.Len() {
            if self.types[i].name == *name {
                return Some(self.types[i].clone());
            }
            i += 1;
        }
        None
    }
}

// ─── reading JSON without a struct decoder ────────────────────────────

/// The value at `key`, or null when there is none — including when `value` is
/// not an object at all.
fn member(value: &json::Value, key: &str) -> json::Value {
    match value.AsObject() {
        Some(object) => {
            let (found, ok) = object.Get(key);
            if ok {
                found
            } else {
                json::Value::Null
            }
        }
        None => json::Value::Null,
    }
}

/// The string at `key`, or empty when it is absent or null.
fn member_string(value: &json::Value, key: &str) -> string {
    match member(value, key).AsString() {
        Some(found) => found.clone(),
        None => string(""),
    }
}

/// The array at `key`, or empty when it is absent or null.
fn member_array(value: &json::Value, key: &str) -> slice<json::Value> {
    match member(value, key).AsArray() {
        Some(found) => found.clone(),
        None => make!([]json::Value, 0, 0),
    }
}

// ─── decoding ─────────────────────────────────────────────────────────

/// Flatten a `__Type` reference chain.
fn read_type_ref(value: &json::Value) -> TypeRef {
    let mut current = value.clone();
    let mut list = false;
    let non_null = member_string(&current, "kind") == "NON_NULL";

    // Walk to the innermost named type, remembering whether a list was crossed.
    loop {
        let kind = member_string(&current, "kind");
        if kind != "NON_NULL" && kind != "LIST" {
            return TypeRef {
                name: member_string(&current, "name"),
                kind,
                list,
                non_null,
            };
        }
        if kind == "LIST" {
            list = true;
        }
        let next = member(&current, "ofType");
        if next.IsNull() {
            // A wrapper with nothing inside it is a malformed schema rather
            // than a shape to handle; naming no type makes the renderer skip
            // the field instead of emitting something that will not compile.
            return TypeRef {
                name: string(""),
                kind: string(""),
                list,
                non_null,
            };
        }
        current = next;
    }
}

fn read_input_value(value: &json::Value) -> InputValue {
    InputValue {
        name: member_string(value, "name"),
        doc: member_string(value, "description"),
        ty: read_type_ref(&member(value, "type")),
        default_value: member_string(value, "defaultValue"),
    }
}

fn read_input_values(value: &json::Value, key: &str) -> slice<InputValue> {
    let raw = member_array(value, key);
    let mut out = make!([]InputValue, 0, raw.Len());
    let mut i: goish::int = 0;
    while i < raw.Len() {
        out = append!(out, read_input_value(&raw[i]));
        i += 1;
    }
    out
}

fn read_field(value: &json::Value) -> Field {
    // `deprecationReason` is null on a current field and, for a deprecated one
    // with no reason given, also null — so the flag is what says which.
    let deprecated = if member(value, "isDeprecated").AsBool() == Some(true) {
        let reason = member_string(value, "deprecationReason");
        if reason.Len() == 0 {
            string("deprecated in the engine's schema")
        } else {
            reason
        }
    } else {
        string("")
    };

    Field {
        name: member_string(value, "name"),
        doc: member_string(value, "description"),
        ty: read_type_ref(&member(value, "type")),
        args: read_input_values(value, "args"),
        deprecated,
    }
}

fn read_type(value: &json::Value) -> Type {
    let raw_fields = member_array(value, "fields");
    let mut fields = make!([]Field, 0, raw_fields.Len());
    let mut i: goish::int = 0;
    while i < raw_fields.Len() {
        fields = append!(fields, read_field(&raw_fields[i]));
        i += 1;
    }

    let raw_values = member_array(value, "enumValues");
    let mut enum_values = make!([]EnumValue, 0, raw_values.Len());
    let mut i: goish::int = 0;
    while i < raw_values.Len() {
        enum_values = append!(
            enum_values,
            EnumValue {
                name: member_string(&raw_values[i], "name"),
                doc: member_string(&raw_values[i], "description"),
            }
        );
        i += 1;
    }

    Type {
        kind: member_string(value, "kind"),
        name: member_string(value, "name"),
        doc: member_string(value, "description"),
        fields,
        input_fields: read_input_values(value, "inputFields"),
        enum_values,
    }
}

/// Parse an introspection response.
///
/// Both shapes the engine hands out are accepted: the bare `{"__schema": …}`
/// that `introspectionSchemaJSON` returns, and the `{"data": {"__schema": …}}`
/// a raw GraphQL reply is wrapped in. Which one arrives depends on who ran the
/// query, and telling them apart is one lookup.
pub fn parse(raw: &slice<goish::byte>) -> (Schema, error) {
    let empty = || Schema {
        types: make!([]Type, 0, 0),
        query_type: string(""),
    };

    let mut document = json::Value::Null;
    let err = json::Unmarshal(raw, &mut document);
    if err != goish::nil {
        return (empty(), err);
    }

    let mut root = member(&document, "__schema");
    if root.IsNull() {
        root = member(&member(&document, "data"), "__schema");
    }
    if root.IsNull() {
        return (
            empty(),
            errors::New(string("introspection response has no __schema")),
        );
    }

    let raw_types = member_array(&root, "types");
    if raw_types.Len() == 0 {
        return (
            empty(),
            errors::New(string("introspection schema declares no types")),
        );
    }

    let mut types = make!([]Type, 0, raw_types.Len());
    let mut i: goish::int = 0;
    while i < raw_types.Len() {
        types = append!(types, read_type(&raw_types[i]));
        i += 1;
    }

    // Sorted by name so the output is a function of the schema's content rather
    // than of the order the engine happened to serialise it in. Enum values are
    // deliberately left in schema order: their order is the engine's to choose,
    // and `names::enum_variant` does not depend on it.
    types.sort_unstable_by(|a, b| a.name.as_bytes().cmp(b.name.as_bytes()));

    let query_type = {
        let named = member_string(&member(&root, "queryType"), "name");
        if named.Len() == 0 {
            string("Query")
        } else {
            named
        }
    };

    (Schema { types, query_type }, goish::nil.into())
}

/// Parse an introspection response held as text.
pub fn parse_string(raw: &string) -> (Schema, error) {
    parse(&bytes(raw.clone()))
}
