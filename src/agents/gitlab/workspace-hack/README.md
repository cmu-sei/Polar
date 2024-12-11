#TODO: review and edit for accuracy
https://crates.io/crates/cargo-hakari

https://docs.rs/cargo-hakari/0.9.33/cargo_hakari/

# Workspace Hack Crate

## Overview

This directory contains a "workspace hack" crate, a lightweight crate used to simplify and enhance the build process for this Rust workspace. While it may seem unconventional, this crate serves an important purpose in managing the dependencies and behavior of the entire project.

---

## What is a Workspace Hack?

A workspace hack crate is a placeholder or utility crate included in a Rust workspace for one or more of the following reasons:

1. **Centralizing Dependencies**: It allows us to consolidate and share dependencies across the workspace, avoiding duplication and ensuring consistency.
2. **Forcing Compilation Order**: It can help control the order in which certain crates are built or tested in the workspace.
3. **Tool Integration**: Some tools or workflows expect a crate at the root of the workspace. A workspace hack ensures compatibility.

---

## Why Does This Exist?

In this project, the workspace hack crate exists to:

- **Encourage Shared Dependency Management**: Dependencies declared here are made available to other workspace crates without duplicating versions or configurations.
- **Optimize CI/CD**: It ensures that all workspace crates are tested and built in a predictable order.
- **Support External Tools**: Some Rust tools require a "default" crate to operate smoothly, and this crate fulfills that role.

---

## How to Use It

1. **Adding Dependencies**  
   Declare common dependencies in the `Cargo.toml` file of the workspace hack crate. These will propagate to the other workspace crates.

   Example:
   ```toml
   [dependencies]
   serde = "1.0"
   tokio = { version = "1.28", features = ["full"] }
   ```

2. **Building the Workspace**  
   Use `cargo build` as usual. The workspace hack crate ensures all other crates build with shared dependencies.

3. **Testing All Crates**  
   Run `cargo test` at the root of the workspace. The workspace hack crate guarantees the proper inclusion of all workspace crates.

---

## Notes

- **No Production Code**: The workspace hack crate does not contain any production code. It is purely a utility for development and build management.
- **Minimal Dependencies**: Keep dependencies limited to what’s necessary for the workspace to function.

---

By understanding and utilizing the workspace hack crate effectively, we can maintain a well-organized, efficient, and cohesive Rust workspace. 🚀