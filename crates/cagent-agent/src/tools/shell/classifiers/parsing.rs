use super::super::*;

#[derive(Default)]
pub(super) struct ParsedArgs {
    pub(super) positionals: Vec<String>,
    pub(super) path_values: Vec<(String, &'static str)>,
    pub(super) options: BTreeSet<String>,
    pub(super) option_values: Vec<(String, String)>,
}

impl ParsedArgs {
    pub(super) fn option_value(&self, names: &[&str]) -> Option<String> {
        self.option_values
            .iter()
            .find(|(name, _)| names.contains(&name.as_str()))
            .map(|(_, value)| value.clone())
    }
}

pub(super) struct OptionGrammar<'a> {
    pub(super) short_flags: &'a str,
    pub(super) short_values: &'a str,
    pub(super) long_flags: &'a [&'a str],
    pub(super) long_values: &'a [&'a str],
    pub(super) path_long_values: &'a [(&'a str, &'static str)],
    pub(super) path_short_values: &'a [(char, &'static str)],
}

pub(super) fn parse_options(args: &[String], grammar: &OptionGrammar<'_>) -> Option<ParsedArgs> {
    let mut parsed = ParsedArgs::default();
    let mut index = 0;
    let mut options = true;
    while index < args.len() {
        let arg = &args[index];
        if options && arg == "--" {
            options = false;
            index += 1;
            continue;
        }
        if options && arg.starts_with("--") {
            let (name, attached) = arg
                .split_once('=')
                .map_or((arg.as_str(), None), |(name, value)| (name, Some(value)));
            if grammar.long_flags.contains(&name) {
                if attached.is_some() {
                    return None;
                }
                parsed.options.insert(name.into());
            } else if grammar.long_values.contains(&name)
                || grammar
                    .path_long_values
                    .iter()
                    .any(|(candidate, _)| *candidate == name)
            {
                let value = if let Some(value) = attached {
                    if value.is_empty() {
                        return None;
                    }
                    value.to_owned()
                } else {
                    index += 1;
                    args.get(index)?.clone()
                };
                parsed.options.insert(name.into());
                parsed.option_values.push((name.into(), value.clone()));
                if let Some((_, source)) = grammar
                    .path_long_values
                    .iter()
                    .find(|(candidate, _)| *candidate == name)
                {
                    parsed.path_values.push((value, *source));
                }
            } else {
                return None;
            }
            index += 1;
            continue;
        }
        if options && arg.starts_with('-') && arg != "-" {
            let chars = arg[1..].char_indices().collect::<Vec<_>>();
            if chars.is_empty() {
                return None;
            }
            let mut consumed_value = false;
            for (position, (offset, option)) in chars.iter().enumerate() {
                if grammar.short_flags.contains(*option) {
                    parsed.options.insert(format!("-{option}"));
                    continue;
                }
                if grammar.short_values.contains(*option)
                    || grammar
                        .path_short_values
                        .iter()
                        .any(|(candidate, _)| candidate == option)
                {
                    let value_start = 1 + offset + option.len_utf8();
                    let value = if position + 1 < chars.len() {
                        arg[value_start..].to_owned()
                    } else {
                        index += 1;
                        args.get(index)?.clone()
                    };
                    if value.is_empty() {
                        return None;
                    }
                    let name = format!("-{option}");
                    parsed.options.insert(name.clone());
                    parsed.option_values.push((name, value.clone()));
                    if let Some((_, source)) = grammar
                        .path_short_values
                        .iter()
                        .find(|(candidate, _)| candidate == option)
                    {
                        parsed.path_values.push((value, *source));
                    }
                    consumed_value = true;
                    break;
                }
                return None;
            }
            let _ = consumed_value;
        } else {
            parsed.positionals.push(arg.clone());
        }
        index += 1;
    }
    Some(parsed)
}

pub(super) fn input_parse(
    parsed: ParsedArgs,
    presentation: SafeShellPresentation,
    min: usize,
    max: Option<usize>,
    default_dot: bool,
) -> Option<SafeCommandParse> {
    if parsed.positionals.len() < min || max.is_some_and(|max| parsed.positionals.len() > max) {
        return None;
    }
    let mut result = SafeCommandParse::new(Some(presentation));
    for (value, source) in parsed.path_values {
        result = result.path(value, source);
    }
    for value in parsed.positionals {
        result = result.path(value, "positional input");
    }
    if result.operands.is_empty() && default_dot {
        result = result.path(".", "default current directory");
    }
    if result.operands.is_empty() && presentation == SafeShellPresentation::Read {
        result.presentation = None;
        result.transparent = true;
    }
    Some(result)
}

pub(super) fn label_paths(mut parsed: SafeCommandParse, label: &str) -> SafeCommandParse {
    parsed.detail = Some(SafePresentationDetail::new(label).arguments(
        parsed.operands.iter().map(|operand| operand.value.clone()),
        true,
    ));
    parsed
}

pub(super) const EMPTY: &[&str] = &[];
pub(super) const EMPTY_PATH_LONG: &[(&str, &str)] = &[];
pub(super) const EMPTY_PATH_SHORT: &[(char, &str)] = &[];

pub(super) fn grammar<'a>(
    short_flags: &'a str,
    short_values: &'a str,
    long_flags: &'a [&'a str],
    long_values: &'a [&'a str],
) -> OptionGrammar<'a> {
    OptionGrammar {
        short_flags,
        short_values,
        long_flags,
        long_values,
        path_long_values: EMPTY_PATH_LONG,
        path_short_values: EMPTY_PATH_SHORT,
    }
}
