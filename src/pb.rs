//! Message types generated from proto/ by build.rs.
#![allow(clippy::all, dead_code)]

include!(concat!(env!("OUT_DIR"), "/_all.rs"));

/// Every message's descriptor (built in by build.rs), for JSON and reflection.
pub fn pool() -> &'static prost_reflect::DescriptorPool {
    static POOL: std::sync::OnceLock<prost_reflect::DescriptorPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        prost_reflect::DescriptorPool::decode(&include_bytes!(concat!(env!("OUT_DIR"), "/descriptors.bin"))[..])
            .expect("built-in descriptors")
    })
}
