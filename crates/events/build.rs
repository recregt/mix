use protox::prost::Message;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto");
    let descriptors = protox::compile(["mix/events/v1/events.proto"], ["proto"])?;
    let encoded = descriptors.encode_to_vec();

    prost_build::Config::new()
        .compile_well_known_types()
        .extern_path(".google.protobuf", "::pbjson_types")
        .compile_fds(descriptors)?;

    pbjson_build::Builder::new()
        .register_descriptors(&encoded)?
        .ignore_unknown_fields()
        .ignore_unknown_enum_variants()
        .build(&[".mix.events.v1"])?;

    Ok(())
}
