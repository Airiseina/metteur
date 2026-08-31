//! Compiles the Metteur protobuf definitions into Rust code.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = std::path::Path::new("../../schema/proto");
    let proto_file = proto_dir.join("metteur.proto");

    println!("cargo:rerun-if-changed={}", proto_file.display());

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&[proto_file], &[proto_dir.to_path_buf()])?;
    Ok(())
}
