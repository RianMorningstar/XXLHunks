//! Generates Rust types for every message in proto/ (protox compiles the files, so protoc is not needed).

fn main() {
    let dir = std::path::Path::new("proto");
    println!("cargo:rerun-if-changed=proto");
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("proto dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".proto"))
        .collect();
    names.sort();
    let fds = protox::compile(&names, [dir]).expect("compile protos");
    // the descriptors themselves too, so the server needs no proto folder at run time (pb::pool)
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    std::fs::write(out.join("descriptors.bin"), prost::Message::encode_to_vec(&fds)).expect("write descriptors");
    prost_build::Config::new()
        .include_file("_all.rs")
        .compile_fds(fds)
        .expect("generate Rust types");
}
