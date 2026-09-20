//! The three scalars the static schema declared beyond async-graphql's built-ins (`GSR-SCALARS`).
//! `Required<T>` (`GSR-REQUIRED`) lands with `deployment.rs`, its only consumer. None goes through
//! Seaography's own scalar machinery
//! (`GqlScalarValueType`, `custom/impls.rs`): that machinery exists to map a real SeaORM column
//! type onto a GraphQL scalar by a fixed name (confirmed by reading `custom/impls.rs` directly —
//! it already implements `CustomOutputType`/`CustomInputType` for `serde_json::Value` itself, but
//! names it `"Json"`, not the `"JSON"` this contract's frozen scalar is spelled; relying on it
//! would silently rename the wire scalar). These three hand-write the same two traits directly,
//! naming exactly what the contract names.

use async_graphql::dynamic::{FieldValue, TypeRef, ValueAccessor};
use seaography::{BuilderContext, CustomInputType, CustomOutputType, SeaResult, SeaographyError};

/// The standard GraphQL `ID` scalar. Seaography has no impl for `async_graphql::ID`
/// (`GSR-FACT-DERIVES`) and a plain `String` would register as GraphQL `String`, not `ID`
/// (`custom/impls.rs`'s `impl_scalar_type!(String)` names it via `GqlScalarValueType`, which always
/// answers `"String"` for a Rust `String`); every id-shaped field in this schema needs `ID`.
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Id(pub String);

impl From<String> for Id {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Id {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl CustomOutputType for Id {
    fn gql_output_type_ref(_ctx: &'static BuilderContext) -> TypeRef {
        TypeRef::named_nn(TypeRef::ID)
    }

    fn gql_field_value(self, _ctx: &'static BuilderContext) -> Option<FieldValue<'static>> {
        Some(FieldValue::value(self.0))
    }
}

impl CustomInputType for Id {
    fn gql_input_type_ref(_ctx: &'static BuilderContext) -> TypeRef {
        TypeRef::named_nn(TypeRef::ID)
    }

    fn parse_value(
        _ctx: &'static BuilderContext,
        value: Option<ValueAccessor<'_>>,
    ) -> SeaResult<Self> {
        match value {
            None => Err(SeaographyError::AsyncGraphQLError("Value expected".into())),
            Some(value) => Ok(Id(value.string()?.to_string())),
        }
    }
}

/// A signed 64-bit integer. Serializes as a JSON number, never a string
/// (`RTD-SCALAR-SERIALIZATION`), and accepts what the static schema's `Long` accepted: an integer,
/// or a string holding one (`schema/long.rs`, ported verbatim below).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Long(pub i64);

impl From<i64> for Long {
    fn from(value: i64) -> Self {
        Self(value)
    }
}

impl From<Long> for i64 {
    fn from(value: Long) -> Self {
        value.0
    }
}

impl CustomOutputType for Long {
    fn gql_output_type_ref(_ctx: &'static BuilderContext) -> TypeRef {
        TypeRef::named_nn("Long")
    }

    fn gql_field_value(self, _ctx: &'static BuilderContext) -> Option<FieldValue<'static>> {
        Some(FieldValue::value(async_graphql::Value::Number(
            self.0.into(),
        )))
    }
}

impl CustomInputType for Long {
    fn gql_input_type_ref(_ctx: &'static BuilderContext) -> TypeRef {
        TypeRef::named_nn("Long")
    }

    fn parse_value(
        _ctx: &'static BuilderContext,
        value: Option<ValueAccessor<'_>>,
    ) -> SeaResult<Self> {
        let parsed = match value.map(|value| value.as_value().clone()) {
            Some(async_graphql::Value::Number(number)) => number.as_i64(),
            Some(async_graphql::Value::String(text)) => text.parse().ok(),
            _ => None,
        };
        parsed.map(Long).ok_or_else(|| {
            SeaographyError::AsyncGraphQLError("A signed 64-bit integer is required.".into())
        })
    }
}

/// A structured JSON value, named `JSON` (the frozen contract's spelling; `custom/impls.rs`'s own
/// blanket impl for `serde_json::Value` names it `Json`, one byte off).
#[derive(Clone, Debug, PartialEq)]
pub struct Json(pub serde_json::Value);

impl From<serde_json::Value> for Json {
    fn from(value: serde_json::Value) -> Self {
        Self(value)
    }
}

impl From<Json> for serde_json::Value {
    fn from(value: Json) -> Self {
        value.0
    }
}

impl CustomOutputType for Json {
    fn gql_output_type_ref(_ctx: &'static BuilderContext) -> TypeRef {
        TypeRef::named_nn("JSON")
    }

    fn gql_field_value(self, _ctx: &'static BuilderContext) -> Option<FieldValue<'static>> {
        Some(FieldValue::value(
            async_graphql::Value::from_json(self.0).ok()?,
        ))
    }
}

impl CustomInputType for Json {
    fn gql_input_type_ref(_ctx: &'static BuilderContext) -> TypeRef {
        TypeRef::named_nn("JSON")
    }

    fn parse_value(
        _ctx: &'static BuilderContext,
        value: Option<ValueAccessor<'_>>,
    ) -> SeaResult<Self> {
        match value {
            None => Err(SeaographyError::AsyncGraphQLError("Value expected".into())),
            Some(value) => Ok(Json(value.deserialize()?)),
        }
    }
}

/// Wraps an optional `String` in a field the schema declares non-null, matching the static tier's
/// `required.rs`: a missing value fails with the engine's own null-propagation error
/// (`GSR-FACT-NULL`, `internal: non-null types require a return value`) rather than this crate
/// composing its own message, since the dynamic schema's non-null handling already nulls the
/// nearest nullable ancestor the same way. `deployment.rs` (`packageReference`, `observedAt`,
/// `expiresAt`, `message`) is the only consumer, in both tiers, always wrapping a `String` — a
/// concrete type rather than `Required<T>` sidesteps the same `String`-isn't-`CustomOutputType`
/// split `StringList`'s doc comment explains (a generic `impl<T: CustomOutputType> ...` bound
/// would exclude `Required<String>` for the identical reason), and no other usage ever needs one.
#[derive(Clone)]
pub struct Required(pub Option<String>);

impl CustomOutputType for Required {
    fn gql_output_type_ref(_ctx: &'static BuilderContext) -> TypeRef {
        TypeRef::named_nn(TypeRef::STRING)
    }

    fn gql_field_value(self, _ctx: &'static BuilderContext) -> Option<FieldValue<'static>> {
        self.0.map(FieldValue::value)
    }
}

/// A `[String!]!` field. `#[derive(CustomOutputType)]` cannot express a bare `Vec<String>` field:
/// Seaography's blanket `impl<T: ValueType + Into<sea_orm::Value>> GqlScalarValueType for T`
/// (`ValueType` from SeaORM's query-building layer) also covers `Vec<String>` itself (SeaORM
/// supports it as a Postgres array column type), and that blanket wins the field's type-ref
/// resolution over
/// `CustomOutputType`'s own `impl<T: CustomOutputType> CustomOutputType for Vec<T>` (which isn't
/// satisfied anyway, since plain `String` implements `GqlScalarValueType`, not
/// `CustomOutputType`). At runtime this panics: `TypesMapHelper` has no GraphQL mapping for the
/// resulting SeaORM `ColumnType::Array`, so `GqlScalarValueType::gql_type_ref` hits its own
/// `unreachable!("{} is not handled", T::type_name())` — confirmed with a throwaway
/// `#[derive(CustomOutputType)] struct { items: Vec<String> }`, which panics
/// `"Vec<T> is not handled"` the moment `basic_object` builds its field list. `#[derive(
/// CustomInputType)]` hits the identical panic for a `Vec<String>` *input* field the same way
/// (`GqlScalarValueType::gql_input_type_ref` shares the same default body), so both directions are
/// implemented by hand below. This newtype isn't a SeaORM `ValueType`, so it only ever resolves
/// through these hand-written impls.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StringList(pub Vec<String>);

impl From<Vec<String>> for StringList {
    fn from(value: Vec<String>) -> Self {
        Self(value)
    }
}

impl CustomOutputType for StringList {
    fn gql_output_type_ref(_ctx: &'static BuilderContext) -> TypeRef {
        TypeRef::NonNull(Box::new(TypeRef::List(Box::new(TypeRef::named_nn(
            TypeRef::STRING,
        )))))
    }

    fn gql_field_value(self, _ctx: &'static BuilderContext) -> Option<FieldValue<'static>> {
        Some(FieldValue::list(self.0.into_iter().map(FieldValue::value)))
    }
}

impl CustomInputType for StringList {
    fn gql_input_type_ref(_ctx: &'static BuilderContext) -> TypeRef {
        TypeRef::NonNull(Box::new(TypeRef::List(Box::new(TypeRef::named_nn(
            TypeRef::STRING,
        )))))
    }

    fn parse_value(
        _ctx: &'static BuilderContext,
        value: Option<ValueAccessor<'_>>,
    ) -> SeaResult<Self> {
        let Some(value) = value else {
            return Err(SeaographyError::AsyncGraphQLError(
                "Expected a list, got missing value".into(),
            ));
        };
        let list = value.list()?;
        let mut items = Vec::with_capacity(list.len());
        for item in list.iter() {
            items.push(item.string()?.to_string());
        }
        Ok(StringList(items))
    }
}

/// Hand-rolls `CustomOutputType`/`CustomInputType` for a plain, unit-variant-only enum whose Rust
/// variant identifiers spell the wire enum values verbatim (`GSR-WIRE-CASE` — e.g.
/// `AGENT_VERSION`, not `AgentVersion`, since the frozen contract's enum values are
/// SCREAMING_SNAKE_CASE). `#[derive(CustomEnum)]` (from `seaography_macros`) only builds the
/// enum's own `to_enum()` *type definition* (for `register_custom_enum`, matching how
/// `register_custom_output`/`register_custom_input` work) — confirmed by reading
/// `custom_enum.rs` directly, no blanket impl bridges `CustomEnum` to either trait a struct field
/// or a resolver argument/return type actually needs to use the enum. Every enum type still needs
/// its own `#[derive(CustomEnum)]` for registration; this macro supplies the other two traits.
macro_rules! wire_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        impl seaography::CustomOutputType for $name {
            fn gql_output_type_ref(
                _ctx: &'static seaography::BuilderContext,
            ) -> async_graphql::dynamic::TypeRef {
                async_graphql::dynamic::TypeRef::named_nn(stringify!($name))
            }

            fn gql_field_value(
                self,
                _ctx: &'static seaography::BuilderContext,
            ) -> Option<async_graphql::dynamic::FieldValue<'static>> {
                let name = match self {
                    $($name::$variant => stringify!($variant),)+
                };
                Some(async_graphql::dynamic::FieldValue::value(
                    async_graphql::Value::Enum(async_graphql::Name::new(name)),
                ))
            }
        }

        impl seaography::CustomInputType for $name {
            fn gql_input_type_ref(
                _ctx: &'static seaography::BuilderContext,
            ) -> async_graphql::dynamic::TypeRef {
                async_graphql::dynamic::TypeRef::named_nn(stringify!($name))
            }

            fn parse_value(
                _ctx: &'static seaography::BuilderContext,
                value: Option<async_graphql::dynamic::ValueAccessor<'_>>,
            ) -> seaography::SeaResult<Self> {
                let value = value.ok_or_else(|| {
                    seaography::SeaographyError::AsyncGraphQLError("Expected a value".into())
                })?;
                match value.enum_name()? {
                    $(stringify!($variant) => Ok($name::$variant),)+
                    other => Err(seaography::SeaographyError::AsyncGraphQLError(
                        format!("Unrecognized {} value `{other}`", stringify!($name)).into(),
                    )),
                }
            }
        }
    };
}
pub(crate) use wire_enum;

// `GSR-NULL-ARGUMENT`: `ObjectAccessor::get` (async-graphql's dynamic argument accessor) returns
// `Some(ValueAccessor(Value::Null))`, not `None`, when a client passes an optional argument
// explicitly as `null` — a variable bound to null, or a literal `null` — rather than omitting it;
// only a truly *absent* key yields `None` (confirmed by reading `dynamic::value_accessor.rs`'s
// `ObjectAccessor::get`/`try_get` directly). Every hand-built resolver in this schema that reads an
// optional argument must treat "present but null" the same as "absent", or a client's explicit
// `null` (a common pattern, e.g. an unset GraphQL variable) crashes with `.string()`/`.i64()`/
// `.boolean()`'s own "internal: not a ..." error instead of being read as a missing value.
pub(crate) fn defined(value: Option<ValueAccessor<'_>>) -> Option<ValueAccessor<'_>> {
    value.filter(|value| !value.is_null())
}

pub(crate) fn optional_string(
    value: Option<ValueAccessor<'_>>,
) -> async_graphql::Result<Option<String>> {
    defined(value)
        .map(|value| value.string().map(str::to_string))
        .transpose()
}

#[cfg(test)]
mod tests {
    //! Exercises each scalar through a real, throwaway schema: this proves the printed SDL names
    //! it correctly *and* that a value round-trips through the actual GraphQL engine, rather than
    //! poking `CustomOutputType`/`CustomInputType` directly (`ValueAccessor` has no public
    //! constructor outside the engine's own request path, so a hand-built one is not available;
    //! executing a real query is also the more faithful test of what a client actually sees).
    use super::*;
    use async_graphql::dynamic::{Field, FieldFuture, InputValue, Object, Schema, TypeRef};
    use seaography::async_graphql;
    use std::sync::LazyLock;

    static CONTEXT: LazyLock<BuilderContext> = LazyLock::new(BuilderContext::default);

    fn schema() -> Schema {
        let query = Object::new("Query")
            .field(
                Field::new("longEcho", TypeRef::named_nn("Long"), |ctx| {
                    FieldFuture::new(async move {
                        let value = Long::parse_value(&CONTEXT, ctx.args.get("value"))?;
                        Ok(value.gql_field_value(&CONTEXT))
                    })
                })
                .argument(InputValue::new("value", TypeRef::named_nn("Long"))),
            )
            .field(
                Field::new("idEcho", TypeRef::named_nn(TypeRef::ID), |ctx| {
                    FieldFuture::new(async move {
                        let value = Id::parse_value(&CONTEXT, ctx.args.get("value"))?;
                        Ok(value.gql_field_value(&CONTEXT))
                    })
                })
                .argument(InputValue::new("value", TypeRef::named_nn(TypeRef::ID))),
            )
            .field(
                Field::new("jsonEcho", TypeRef::named_nn("JSON"), |ctx| {
                    FieldFuture::new(async move {
                        let value = Json::parse_value(&CONTEXT, ctx.args.get("value"))?;
                        Ok(value.gql_field_value(&CONTEXT))
                    })
                })
                .argument(InputValue::new("value", TypeRef::named_nn("JSON"))),
            )
            .field(
                Field::new(
                    "stringListEcho",
                    StringList::gql_output_type_ref(&CONTEXT),
                    |ctx| {
                        FieldFuture::new(async move {
                            let value = StringList::parse_value(&CONTEXT, ctx.args.get("value"))?;
                            Ok(value.gql_field_value(&CONTEXT))
                        })
                    },
                )
                .argument(InputValue::new(
                    "value",
                    StringList::gql_input_type_ref(&CONTEXT),
                )),
            )
            .field(
                Field::new(
                    "optionalStringEcho",
                    TypeRef::named_nn(TypeRef::STRING),
                    |ctx| {
                        FieldFuture::new(async move {
                            let value = optional_string(ctx.args.get("value"))?;
                            Ok(Some(FieldValue::value(
                                value.unwrap_or_else(|| "ABSENT".to_string()),
                            )))
                        })
                    },
                )
                .argument(InputValue::new("value", TypeRef::named(TypeRef::STRING))),
            );
        Schema::build("Query", None, None)
            .register(query)
            .register(async_graphql::dynamic::Scalar::new("Long"))
            .register(async_graphql::dynamic::Scalar::new("JSON"))
            .finish()
            .expect("the scalar smoke-test schema composes")
    }

    #[tokio::test]
    async fn long_prints_as_its_own_scalar_and_round_trips_a_large_number() {
        let sdl = schema().sdl();
        assert!(sdl.contains("scalar Long"), "{sdl}");
        assert!(sdl.contains("longEcho(value: Long!): Long!"), "{sdl}");

        let response = schema().execute("{ longEcho(value: 5000000000) }").await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"longEcho": 5_000_000_000_i64})
        );

        // The static tier's `Long` also accepted a numeric string; this one does too.
        let from_string = schema()
            .execute(r#"{ longEcho(value: "5000000000") }"#)
            .await;
        assert!(from_string.errors.is_empty(), "{:?}", from_string.errors);
    }

    #[tokio::test]
    async fn long_rejects_a_non_numeric_string() {
        let response = schema().execute(r#"{ longEcho(value: "five") }"#).await;
        assert!(!response.errors.is_empty());
    }

    #[tokio::test]
    async fn id_prints_as_the_standard_id_scalar() {
        let sdl = schema().sdl();
        assert!(sdl.contains("idEcho(value: ID!): ID!"), "{sdl}");

        let response = schema()
            .execute(r#"{ idEcho(value: "10000000-0000-0000-0000-000000000001") }"#)
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"idEcho": "10000000-0000-0000-0000-000000000001"})
        );
    }

    #[tokio::test]
    async fn json_prints_uppercase_and_round_trips_a_structured_value() {
        let sdl = schema().sdl();
        assert!(sdl.contains("scalar JSON"), "{sdl}");
        assert!(!sdl.contains("scalar Json\n"), "{sdl}");

        // A JSON scalar argument is a real GraphQL literal (an object/array/number/etc.), not a
        // JSON-encoded string; a query never spells JSON text as one long escaped string.
        let response = schema()
            .execute(r#"{ jsonEcho(value: {a: 1, b: [true, null]}) }"#)
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"jsonEcho": {"a": 1, "b": [true, null]}})
        );
    }

    #[tokio::test]
    async fn string_list_prints_as_a_non_null_list_of_non_null_strings_and_round_trips() {
        let sdl = schema().sdl();
        assert!(
            sdl.contains("stringListEcho(value: [String!]!): [String!]!"),
            "{sdl}"
        );

        let response = schema()
            .execute(r#"{ stringListEcho(value: ["a", "b"]) }"#)
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"stringListEcho": ["a", "b"]})
        );
    }

    #[tokio::test]
    async fn string_list_rejects_a_non_list_value() {
        let response = schema()
            .execute(r#"{ stringListEcho(value: "not a list") }"#)
            .await;
        assert!(!response.errors.is_empty());
    }

    #[allow(non_camel_case_types)]
    #[derive(seaography::CustomEnum, Clone, Copy, PartialEq, Eq, Debug)]
    enum WireEnumFixture {
        FIRST_VALUE,
        SECOND_VALUE,
    }
    wire_enum!(WireEnumFixture {
        FIRST_VALUE,
        SECOND_VALUE
    });

    fn enum_schema() -> Schema {
        let query = Object::new("Query").field(
            Field::new(
                "wireEnumEcho",
                TypeRef::named_nn("WireEnumFixture"),
                |ctx| {
                    FieldFuture::new(async move {
                        let value = WireEnumFixture::parse_value(&CONTEXT, ctx.args.get("value"))?;
                        Ok(value.gql_field_value(&CONTEXT))
                    })
                },
            )
            .argument(InputValue::new(
                "value",
                TypeRef::named_nn("WireEnumFixture"),
            )),
        );
        Schema::build("Query", None, None)
            .register(query)
            .register(<WireEnumFixture as seaography::CustomEnum>::to_enum())
            .finish()
            .expect("the enum smoke-test schema composes")
    }

    #[tokio::test]
    async fn wire_enum_prints_variants_verbatim_and_round_trips() {
        let sdl = enum_schema().sdl();
        assert!(
            sdl.contains("enum WireEnumFixture {\n\tFIRST_VALUE\n\tSECOND_VALUE\n}"),
            "{sdl}"
        );

        let response = enum_schema()
            .execute("{ wireEnumEcho(value: SECOND_VALUE) }")
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"wireEnumEcho": "SECOND_VALUE"})
        );
    }

    #[tokio::test]
    async fn wire_enum_rejects_an_unrecognized_value() {
        let response = enum_schema()
            .execute("{ wireEnumEcho(value: THIRD_VALUE) }")
            .await;
        assert!(!response.errors.is_empty());
    }

    // `GSR-NULL-ARGUMENT`: an omitted argument and an explicit `null` argument must resolve the
    // same way. `check:integration:approval` caught a resolver treating them differently
    // (`ctx.args.get(name).map(|value| value.string()...)` called `.string()` on an explicit
    // `Value::Null`, since `.get()` returns `Some(...)` for a present-but-null key, not `None`).
    #[tokio::test]
    async fn optional_string_treats_an_omitted_argument_as_absent() {
        let response = schema().execute("{ optionalStringEcho }").await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"optionalStringEcho": "ABSENT"})
        );
    }

    #[tokio::test]
    async fn optional_string_treats_an_explicit_null_argument_as_absent() {
        let response = schema()
            .execute("{ optionalStringEcho(value: null) }")
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"optionalStringEcho": "ABSENT"})
        );

        // The same must hold when `null` arrives via a variable, not just a literal — the query
        // this port's own regression (`scripts/approval-access.mjs`) sent `{ after: null }` as a
        // variable, not a literal `null` in the document text.
        let via_variable = schema()
            .execute(
                async_graphql::Request::new("query($v: String) { optionalStringEcho(value: $v) }")
                    .variables(async_graphql::Variables::from_json(
                        serde_json::json!({"v": null}),
                    )),
            )
            .await;
        assert!(via_variable.errors.is_empty(), "{:?}", via_variable.errors);
        assert_eq!(
            via_variable.data.into_json().unwrap(),
            serde_json::json!({"optionalStringEcho": "ABSENT"})
        );
    }

    #[tokio::test]
    async fn optional_string_returns_a_present_value() {
        let response = schema()
            .execute(r#"{ optionalStringEcho(value: "hello") }"#)
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        assert_eq!(
            response.data.into_json().unwrap(),
            serde_json::json!({"optionalStringEcho": "hello"})
        );
    }
}
