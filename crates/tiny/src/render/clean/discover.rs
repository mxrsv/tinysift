pub use tiny_core::clean::discover::{CategoryGroup, DiscoveryReport};
pub fn discover(opts: &crate::cli::CleanOpts) -> tiny_core::error::Result<DiscoveryReport> {
    tiny_core::clean::discover::discover(&opts.into())
}
