# CapnProto Data Models for Rust

This crate provides Cap’n Proto-generated data types for serialization. It uses the **Cap’n Proto** schema language to define the data models and automatically generates Rust code using the `capnpc` compiler.

## Features

- **Cap’n Proto Integration**: Define schemas in `.capnp` files and generate efficient Rust types.
- **High-Performance Serialization**: Leverage Cap’n Proto's zero-copy serialization for fast and compact data transmission.
- **Flexible Data Modeling**: Includes unions and nested structs for complex relationships.
- **Compatibility**: Easily integrate with other Cap’n Proto-supported languages (e.g., Python, C++).

### Cap’n Proto Compiler
Ensure you have the `capnp` compiler installed on your system:

```sh
sudo apt install capnproto   # Debian/Ubuntu
brew install capnp           # macOS
nix-shell -p capnproto
```

