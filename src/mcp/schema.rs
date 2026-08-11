//! Shared MCP JSON Schema helpers.

pub(crate) fn any_json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({})
}
