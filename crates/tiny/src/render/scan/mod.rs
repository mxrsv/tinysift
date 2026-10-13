mod duplicates;
mod extensions;
mod report_json;
mod report_text;
mod sort;
mod types;
pub fn run(opts: crate::cli::ScanOpts) -> anyhow::Result<()> {
    let data = tiny_core::scan::scan(&(&opts).into(), None)?;
    if opts.json {
        report_json::render(&data, &opts)?;
    } else {
        report_text::render(&data, &opts);
    }
    Ok(())
}
