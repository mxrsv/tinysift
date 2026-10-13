pub fn execute(
    groups: &[&tiny_core::clean::discover::CategoryGroup],
    action: tiny_core::clean::types::CleanAction,
    excluded: &std::collections::HashSet<std::path::PathBuf>,
    opts: &crate::cli::CleanOpts,
) -> tiny_core::error::Result<tiny_core::clean::types::ExecReport> {
    tiny_core::clean::execute::execute(groups, action, excluded, &opts.into())
}
