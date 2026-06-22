// cassini/client/build.rs
fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // cassini/client -> cassini -> agents (workspace root with target/)
    let workspace_root = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap() // cassini/
        .parent()
        .unwrap(); // agents/

    let profile = std::env::var("PROFILE").unwrap();

    let server_bin = workspace_root
        .join("target")
        .join(&profile)
        .join("cassini-server");

    let client_bin = workspace_root
        .join("target")
        .join(&profile)
        .join("cassini-client");

    println!(
        "cargo:rustc-env=CASSINI_SERVER_BIN={}",
        server_bin.display()
    );
    println!(
        "cargo:rustc-env=CASSINI_CLIENT_BIN={}",
        client_bin.display()
    );

    // Re-run if the profile changes, which changes the binary paths.
    println!("cargo:rerun-if-env-changed=PROFILE");
}
