use super::super::*;
use super::parsing::*;

pub(super) fn parse_basename(args: &[String]) -> Option<SafeCommandParse> {
    let parsed = parse_options(
        args,
        &grammar("az", "s", &["--multiple", "--zero"], &["--suffix"]),
    )?;
    let multiple = parsed.options.contains("-a") || parsed.options.contains("--multiple");
    let explicit_suffix = parsed.options.contains("-s") || parsed.options.contains("--suffix");
    let valid_positionals = if multiple {
        !parsed.positionals.is_empty()
    } else if explicit_suffix {
        parsed.positionals.len() == 1
    } else {
        matches!(parsed.positionals.len(), 1 | 2)
    };
    valid_positionals.then(|| SafeCommandParse::new(None))
}
