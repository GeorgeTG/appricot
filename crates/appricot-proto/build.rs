//! Compiles the frozen wire contract into Rust sources for `src/wire.rs`.
//!
//! The include path is `proto/`, so the file resolves as `appricot/v0/wire.proto` and
//! prost emits one module per package component: the generated file defines the contents of
//! `appricot.v0`. The `.proto` files under `proto/` are the frozen wire contract; this script
//! only reads them. `protoc` comes from the dev image (`PROTOC=/usr/bin/protoc`).

use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_file = Path::new("proto")
        .join("appricot")
        .join("v0")
        .join("wire.proto");
    println!("cargo:rerun-if-changed={}", proto_file.display());
    // prost-build reads these to find the compiler and its includes, and says nothing to Cargo
    // about them: without these lines a new protoc under a cached target dir keeps the code
    // the old one generated.
    println!("cargo:rerun-if-env-changed=PROTOC");
    println!("cargo:rerun-if-env-changed=PROTOC_INCLUDE");
    prost_build::Config::new().compile_protos(&[&proto_file], &[Path::new("proto")])?;
    Ok(())
}
