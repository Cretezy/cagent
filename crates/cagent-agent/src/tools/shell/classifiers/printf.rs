use super::super::*;

pub(super) fn parse_printf(args: &[String]) -> Option<SafeCommandParse> {
    let format = args.first()?;
    if format
        .chars()
        .any(|character| matches!(character, '*' | '?' | '{'))
    {
        return None;
    }
    let argument_count = printf_literal_argument_count(format)?;
    if args.len() != argument_count + 1
        || args[1..].iter().any(|argument| {
            argument
                .chars()
                .any(|character| matches!(character, '*' | '?' | '{'))
        })
    {
        return None;
    }
    Some(SafeCommandParse::new(None))
}

fn printf_literal_argument_count(format: &str) -> Option<usize> {
    let mut characters = format.chars();
    let mut arguments = 0;
    while let Some(character) = characters.next() {
        match character {
            '%' => match characters.next()? {
                '%' => {}
                's' => arguments += 1,
                _ => return None,
            },
            '\\' => {
                characters.next()?;
            }
            _ => {}
        }
    }
    Some(arguments)
}
