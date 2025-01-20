fn main() {
        //TODO: There's a cynic_introspect crate available that can download the schema for us, use it based on some config
        cynic_codegen::register_schema("gitlab")
        .from_sdl_file("src/gitlab.graphql")
        .unwrap()
        .as_default()
        .unwrap();
}

