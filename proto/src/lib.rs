use protox::prost::Message;
use tonic_build::FileDescriptorSet;

/// Returns the Protobuf file descriptor for the RPC API.
pub fn rpc_api_descriptor() -> FileDescriptorSet {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/", "rpc_file_descriptor.bin"));
    FileDescriptorSet::decode(&bytes[..])
        .expect("bytes should be a valid file descriptor created by build.rs")
}

/// Returns the Protobuf file descriptor for the remote prover API.
pub fn remote_prover_api_descriptor() -> FileDescriptorSet {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/", "remote_prover_file_descriptor.bin"));
    FileDescriptorSet::decode(&bytes[..])
        .expect("bytes should be a valid file descriptor created by build.rs")
}

/// Returns the Protobuf file descriptor for the store API.
#[cfg(feature = "internal")]
pub fn store_api_descriptor() -> FileDescriptorSet {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/", "store_file_descriptor.bin"));
    FileDescriptorSet::decode(&bytes[..])
        .expect("bytes should be a valid file descriptor created by build.rs")
}

/// Returns the Protobuf file descriptor for the block-producer API.
#[cfg(feature = "internal")]
pub fn block_producer_api_descriptor() -> FileDescriptorSet {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/", "block_producer_file_descriptor.bin"));
    FileDescriptorSet::decode(&bytes[..])
        .expect("bytes should be a valid file descriptor created by build.rs")
}
