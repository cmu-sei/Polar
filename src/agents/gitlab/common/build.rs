//TODO:Build script to generate datatypes using capnproto crate
use capnpc::CompilerCommand;

fn main() {
    CompilerCommand::new()
        // .output_path("/Users/vacoates/projects/Polar/src/agents/proto-build/src/")
        // .src_prefix("schema")
        .file("src/gitlab.capnp")
        .run()
        .expect("Failed to compile Cap'n Proto schema");
}

