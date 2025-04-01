/// Register github schema for creating structs for queries
fn main() {
    cynic_codegen::register_schema("gitlab")
        .from_sdl_file("../schema/src/gitlab.graphql")
        .expect("Failed to find Gitlab GraphQL Schema")
        .as_default()
        .unwrap();
}
