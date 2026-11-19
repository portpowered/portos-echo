/// Shared by the CLI and build script, with no external dependencies.
pub fn normalized(value: &str) -> Option<String> {
    let value = value.strip_prefix('v').unwrap_or(value);
    let (core, pre) = value.split_once('-').map_or((value, None), |(a, b)| (a, Some(b)));
    let numbers: Vec<_> = core.split('.').collect();
    let numeric = |v: &str| !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()) && (v == "0" || !v.starts_with('0'));
    let prerelease = pre.is_none_or(|pre| {
        pre.split('.').all(|part| {
            !part.is_empty()
                && part.bytes().all(|c| c.is_ascii_alphanumeric())
                && (!part.bytes().all(|c| c.is_ascii_digit()) || numeric(part))
        })
    });
    (numbers.len() == 3 && numbers.iter().all(|v| numeric(v)) && prerelease).then(|| value.to_owned())
}
