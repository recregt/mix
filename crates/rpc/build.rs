fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto");
    let descriptors = protox::compile(["mix/worker/v1/worker.proto"], ["proto"])?;
    tonic_prost_build::configure().compile_fds(descriptors)?;
    Ok(())
}
