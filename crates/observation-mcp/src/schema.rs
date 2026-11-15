//! Schemas for tool fields held as untyped JSON.
//!
//! schemars renders a [`serde_json::Value`] field as the boolean schema
//! `true`. That is valid JSON Schema, but some clients that compile tool
//! schemas into grammars reject it: llama.cpp's server refuses every tool
//! when a property schema is not an object. Fields that hold a JSON object,
//! such as a scene file, declare it with `#[schemars(with = "JsonObject")]`
//! (or `Option<JsonObject>`, which stays optional) and keep their `Value`
//! type, so what they accept and return is unchanged.

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use std::borrow::Cow;

/// Stands in for a `Value` field in its schema: any JSON object.
#[derive(Debug)]
pub struct JsonObject;

impl JsonSchema for JsonObject {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("JsonObject")
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "object" })
    }
}
