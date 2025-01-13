//TODO:Build script to generate datatypes using capnproto crate
use capnpc::CompilerCommand;

fn main() {
    CompilerCommand::new()
        .file("src/gitlab.capnp")
        .run()
        .expect("Failed to compile Cap'n Proto schema");
}

