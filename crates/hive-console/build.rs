//! Registers the runtime schema snapshot with cynic, so every query struct in this crate is checked
//! against `schema/hive.graphql` at compile time.

fn main() {
    cynic_codegen::register_schema("hive")
        .from_sdl_file("../../schema/hive.graphql")
        .expect("schema/hive.graphql registers with cynic")
        .as_default()
        .expect("the hive schema becomes the default cynic schema");
}
