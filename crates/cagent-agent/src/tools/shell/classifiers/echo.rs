use super::super::*;

pub(super) fn parse_echo(_args: &[String]) -> Option<SafeCommandParse> {
    // Bash only treats the exact -n/-e/-E forms (and combinations of them) as
    // options. Other leading-dash arguments, such as the common `---` output
    // separator, are literal operands. Shell analysis has already established
    // that every argument is static, so either interpretation is output-only.
    Some(SafeCommandParse::new(None))
}
