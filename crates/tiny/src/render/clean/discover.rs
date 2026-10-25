pub use tiny_core::clean::discover::{CategoryGroup, DiscoveryReport};
pub fn discover(
    opts: &crate::cli::CleanOpts,
) -> tiny_core::error::Result<tiny_core::clean::discover::CliDiscovery> {
    tiny_core::clean::discover::discover_for_cli(&opts.into())
}
