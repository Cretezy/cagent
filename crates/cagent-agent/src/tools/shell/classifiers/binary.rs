use super::super::*;
use super::parsing::*;

pub(super) fn parse_readelf(args: &[String]) -> Option<SafeCommandParse> {
    binary_inputs(
        args,
        &grammar(
            "aheSlrsdgnW",
            "x",
            &[
                "--all",
                "--file-header",
                "--program-headers",
                "--sections",
                "--symbols",
                "--relocs",
                "--dynamic",
                "--notes",
                "--section-groups",
                "--wide",
            ],
            &["--hex-dump"],
        ),
    )
    .map(|parsed| with_label(parsed, "Read ELF"))
}

pub(super) fn parse_nm(args: &[String]) -> Option<SafeCommandParse> {
    binary_inputs(
        args,
        &grammar(
            "ACDglnpPrSuU",
            "t",
            &[
                "--debug-syms",
                "--defined-only",
                "--extern-only",
                "--demangle",
                "--numeric-sort",
                "--no-sort",
                "--print-size",
                "--size-sort",
                "--undefined-only",
            ],
            &["--format", "--radix", "--target"],
        ),
    )
    .map(|parsed| with_label(parsed, "Read symbols"))
}

pub(super) fn parse_objdump(args: &[String]) -> Option<SafeCommandParse> {
    binary_inputs(
        args,
        &grammar(
            "aCdfhprstx",
            "j",
            &[
                "--all-headers",
                "--demangle",
                "--disassemble",
                "--file-headers",
                "--headers",
                "--private-headers",
                "--reloc",
                "--full-contents",
                "--syms",
                "--section-headers",
            ],
            &["--section"],
        ),
    )
    .map(|parsed| with_label(parsed, "Read object"))
}

fn with_label(mut parsed: SafeCommandParse, label: &str) -> SafeCommandParse {
    parsed.detail = Some(SafePresentationDetail::new(label).arguments(
        parsed.operands.iter().map(|operand| operand.value.clone()),
        true,
    ));
    parsed
}

fn binary_inputs(args: &[String], grammar: &OptionGrammar<'_>) -> Option<SafeCommandParse> {
    input_parse(
        parse_options(args, grammar)?,
        SafeShellPresentation::Read,
        1,
        None,
        false,
    )
}
