fn main() {
    if std::env::var_os("PROTOC").is_none() {
        if let Ok(pb) = protoc_bin_vendored::protoc_bin_path() {
            unsafe {
                std::env::set_var("PROTOC", pb);
            }
        }
    }

    prost_build::compile_protos(
        &[
            "proto/device_to_device_messages.proto",
            "proto/offline_wire_formats.proto",
            "proto/securegcm.proto",
            "proto/securemessage.proto",
            "proto/ukey.proto",
            "proto/wire_format.proto",
        ],
        &["proto"],
    )
    .expect("Failed to compile Quick Share protobuf definitions");
}
