//! Registry and metadata for shell command classifiers.

use super::*;

mod base64;
mod basename;
mod binary;
mod cagent;
mod cargo;
mod cat;
mod cd;
mod checksum;
mod cmp;
mod comm;
mod cut;
mod df;
mod diff;
mod dirname;
mod du;
mod echo;
mod expr;
mod fd;
mod file;
mod find;
mod generally_safe;
mod generic;
mod git;
mod grep;
mod head;
mod id;
mod jj;
mod jq;
mod ls;
mod nl;
mod numfmt;
mod parsing;
mod paste;
mod printf;
mod project;
mod pwd;
mod readlink;
mod realpath;
mod rg;
mod sed;
mod seq;
mod simple;
mod sort;
mod sqlite3;
mod stat;
mod strings;
mod tac;
mod tail;
mod tar;
mod test;
mod tr;
mod tree;
mod uname;
mod uniq;
mod unzip;
mod wc;
mod which;

use base64::parse_base64;
use basename::parse_basename;
use binary::{parse_nm, parse_objdump, parse_readelf};
use cagent::parse_cagent;
use cargo::parse_cargo;
use cat::parse_cat;
use cd::parse_cd;
use checksum::parse_checksum;
use cmp::parse_cmp;
use comm::parse_comm;
use cut::parse_cut;
use df::parse_df;
use diff::parse_diff;
use dirname::parse_dirname;
use du::parse_du;
use echo::parse_echo;
use expr::parse_expr;
use fd::parse_fd;
use file::parse_file;
use find::parse_find;
use generic::{parse_date, parse_no_args, parse_whoami};
use git::parse_git;
use grep::parse_grep;
use head::parse_head;
use id::parse_id;
use jj::parse_jj;
use jq::parse_jq;
use ls::parse_ls;
use nl::parse_nl;
use numfmt::parse_numfmt;
use paste::parse_paste;
use printf::parse_printf;
use project::parse_version;
use pwd::parse_pwd;
use readlink::parse_readlink;
use realpath::parse_realpath;
pub(super) use rg::normalize_quoted_leading_dash_pattern;
use rg::parse_rg;
use sed::parse_sed;
use seq::parse_seq;
use simple::parse_simple_inputs;
use sort::parse_sort;
use sqlite3::parse_sqlite3;
use stat::parse_stat;
use strings::parse_strings;
use tac::parse_tac;
use tail::parse_tail;
use tar::parse_tar;
use test::parse_test;
use tr::parse_tr;
use tree::parse_tree;
use uname::parse_uname;
use uniq::parse_uniq;
use unzip::parse_unzip;
use wc::parse_wc;
use which::parse_which;

pub(super) fn classify_generally_safe_segment(
    segment: &super::super::ShellSegment,
) -> Option<ShellSafetyTier> {
    generally_safe::classify_generally_safe_segment(segment)
}

pub(super) fn parse_presentation_invocation(
    spec: &SafeCommandSpec,
    args: &[String],
) -> Option<SafeCommandParse> {
    if spec.canonical == "jj" {
        jj::parse_jj_level1(args)
    } else {
        (spec.parser)(args)
    }
}

pub type SafeCommandParser = fn(&[String]) -> Option<SafeCommandParse>;

#[derive(Clone, Copy)]
pub struct SafeCommandSpec {
    pub canonical: &'static str,
    pub aliases: &'static [&'static str],
    pub platform: SafeCommandPlatform,
    pub builtin: bool,
    pub parser: SafeCommandParser,
    pub examples: &'static [&'static str],
    pub restrictions: &'static str,
    pub category: SafeCommandCategory,
    pub recommends_over: Option<&'static str>,
    pub hardening: SafeCommandHardening,
    pub tier: ShellSafetyTier,
}

macro_rules! spec {
    ($name:literal, $parser:ident, $examples:expr, $restrictions:literal, $category:ident) => {
        SafeCommandSpec {
            canonical: $name,
            aliases: &[],
            platform: SafeCommandPlatform::All,
            builtin: false,
            parser: $parser,
            examples: $examples,
            restrictions: $restrictions,
            category: SafeCommandCategory::$category,
            recommends_over: None,
            hardening: SafeCommandHardening::None,
            tier: ShellSafetyTier::Level0,
        }
    };
}

pub static SAFE_COMMAND_REGISTRY: &[SafeCommandSpec] = &[
    spec!(
        "cagent",
        parse_cagent,
        &["cagent config view --safe", "cagent config list"],
        "only secret-safe, read-only implicit configuration inspection",
        Read
    ),
    spec!(
        "base64",
        parse_base64,
        &["base64 README.md"],
        "display/decode options and explicit inputs only",
        Read
    ),
    spec!(
        "basename",
        parse_basename,
        &["basename src/lib.rs", "basename src/lib.rs .rs"],
        "string transformation only; no filesystem access",
        Generic
    ),
    spec!(
        "cat",
        parse_cat,
        &["cat Cargo.toml", "cat SPEC.md 2>/dev/null"],
        "known display options and input files only",
        Read
    ),
    SafeCommandSpec {
        tier: ShellSafetyTier::Level1,
        ..spec!(
            "cargo",
            parse_cargo,
            &["cargo fmt --check"],
            "strict Cargo verification and metadata forms",
            Read
        )
    },
    spec!(
        "cmp",
        parse_cmp,
        &["cmp old new"],
        "known comparison options and exactly two inputs",
        Read
    ),
    spec!(
        "comm",
        parse_comm,
        &["comm left right"],
        "known display options and exactly two inputs",
        Read
    ),
    SafeCommandSpec {
        builtin: true,
        ..spec!(
            "cd",
            parse_cd,
            &["cd crates && pwd"],
            "exactly one literal directory",
            Generic
        )
    },
    spec!(
        "cut",
        parse_cut,
        &["cut -d: -f1 /etc/passwd"],
        "known selection options and input files only",
        Read
    ),
    spec!(
        "df",
        parse_df,
        &["df -h ."],
        "known metadata options and explicit paths",
        Read
    ),
    spec!("date", parse_date, &["date"], "no arguments", Generic),
    spec!(
        "diff",
        parse_diff,
        &["diff old new"],
        "known comparison options and exactly two inputs",
        Read
    ),
    spec!(
        "dirname",
        parse_dirname,
        &["dirname src/lib.rs"],
        "string transformation only; no filesystem access",
        Generic
    ),
    spec!(
        "du",
        parse_du,
        &["du -h ."],
        "known size-display options and input paths",
        List
    ),
    SafeCommandSpec {
        builtin: true,
        ..spec!(
            "echo",
            parse_echo,
            &["echo hello"],
            "literal operands and -n/-e/-E only",
            Generic
        )
    },
    SafeCommandSpec {
        builtin: true,
        ..spec!(
            "printf",
            parse_printf,
            &["printf README.md"],
            "one literal format with %s/%% directives and matching literal operands; wildcard and brace patterns are unsupported",
            Generic
        )
    },
    spec!(
        "expr",
        parse_expr,
        &["expr 1 + 1"],
        "literal expression operands only",
        Generic
    ),
    SafeCommandSpec {
        builtin: true,
        ..spec!("false", parse_no_args, &["false"], "no arguments", Generic)
    },
    SafeCommandSpec {
        canonical: "fd",
        aliases: &["fdfind"],
        parser: parse_fd,
        examples: &[
            "fd",
            "fd PATTERN src",
            "fd -a 'SPEC.md|Cargo.toml|AGENTS.md' .",
            "fd -e rs . crates",
        ],
        restrictions: "listing/search options only; execution helpers are rejected",
        category: SafeCommandCategory::List,
        recommends_over: Some("find"),
        platform: SafeCommandPlatform::All,
        builtin: false,
        hardening: SafeCommandHardening::None,
        tier: ShellSafetyTier::Level0,
    },
    spec!(
        "file",
        parse_file,
        &["file Cargo.toml"],
        "known metadata options and input files",
        Read
    ),
    spec!(
        "find",
        parse_find,
        &["find src -name '*.rs'"],
        "known predicates and display-only actions",
        Search
    ),
    SafeCommandSpec {
        canonical: "git",
        aliases: &[],
        parser: parse_git,
        examples: &[
            "git status --short",
            "git status --porcelain=v1",
            "git status --short -- plays/say_hi.toml",
            "git log -1 --oneline",
            "git diff -- Cargo.toml",
            "git diff --check -- plays/say_hi.toml",
            "git show --stat HEAD",
            "git branch --list",
            "git ls-files --cached",
            "git grep -n TODO -- crates",
            "git tag --list",
            "git stash list",
            "git worktree list --porcelain",
        ],
        restrictions: "validated status, log, diff, show, file, search, tag, stash, worktree, and listing-only branch forms",
        category: SafeCommandCategory::Generic,
        recommends_over: None,
        platform: SafeCommandPlatform::All,
        builtin: false,
        hardening: SafeCommandHardening::Git,
        tier: ShellSafetyTier::Level0,
    },
    spec!(
        "jj",
        parse_jj,
        &[
            "jj --ignore-working-copy root",
            "jj --ignore-working-copy status",
            "jj --ignore-working-copy --no-pager st",
            "jj --ignore-working-copy file show -r 57df73a66657 crates/cagent-agent/src/presentation/history.rs",
            "jj --ignore-working-copy --no-pager diff -r 57df73a66657 -- crates/cagent-agent/src/presentation/history.rs",
            "jj --ignore-working-copy bookmark list --all",
            "jj --ignore-working-copy operation show @",
            "jj --ignore-working-copy workspace root",
        ],
        "literal inspection-only status, history, diff, and file forms; configuration, templates, tools, and mutation commands are rejected",
        Generic
    ),
    spec!(
        "grep",
        parse_grep,
        &["grep -n TODO src/lib.rs"],
        "known search/display options; all pattern and ignore files are checked",
        Search
    ),
    spec!(
        "head",
        parse_head,
        &["head -n 20 README.md", "head -100 README.md"],
        "known display options and input files",
        Read
    ),
    spec!(
        "id",
        parse_id,
        &["id"],
        "known identity display options and optional user",
        Generic
    ),
    spec!(
        "jq",
        parse_jq,
        &["jq . package.json", "cat package.json | jq -r '.name'"],
        "literal filters, explicit input files, and formatting/input-mode options only; modules and program/input-file options are rejected",
        Read
    ),
    spec!(
        "ls",
        parse_ls,
        &["ls -la"],
        "known display/sort options and operands",
        List
    ),
    spec!(
        "nl",
        parse_nl,
        &["nl -ba README.md"],
        "known numbering options and input files",
        Read
    ),
    spec!(
        "nm",
        parse_nm,
        &["nm --demangle target/debug/cagent"],
        "display-only symbol-table options and explicit binary files",
        Read
    ),
    spec!(
        "objdump",
        parse_objdump,
        &["objdump -h target/debug/cagent"],
        "display-only object-inspection options and explicit binary files",
        Read
    ),
    SafeCommandSpec {
        platform: SafeCommandPlatform::Gnu,
        ..spec!(
            "numfmt",
            parse_numfmt,
            &["numfmt --to=iec 1024"],
            "known numeric formatting options only",
            Generic
        )
    },
    spec!(
        "paste",
        parse_paste,
        &["paste a b"],
        "known delimiter/serial options and input files",
        Read
    ),
    SafeCommandSpec {
        builtin: true,
        ..spec!("pwd", parse_pwd, &["pwd"], "-L or -P only", List)
    },
    spec!(
        "readlink",
        parse_readlink,
        &["readlink link"],
        "known canonicalization/display options and input paths",
        Read
    ),
    spec!(
        "readelf",
        parse_readelf,
        &["readelf -h target/debug/cagent"],
        "display-only ELF-inspection options and explicit binary files",
        Read
    ),
    spec!(
        "realpath",
        parse_realpath,
        &["realpath path"],
        "known canonicalization options and input paths",
        Read
    ),
    spec!(
        "rev",
        parse_simple_inputs,
        &["rev README.md"],
        "input files only",
        Read
    ),
    SafeCommandSpec {
        canonical: "rg",
        aliases: &[],
        parser: parse_rg,
        examples: &["rg --files", "rg needle src"],
        restrictions: "direct search/files forms only; preprocessors, helpers, and archive search are rejected",
        category: SafeCommandCategory::Search,
        recommends_over: Some("grep"),
        platform: SafeCommandPlatform::All,
        builtin: false,
        hardening: SafeCommandHardening::None,
        tier: ShellSafetyTier::Level0,
    },
    spec!(
        "sed",
        parse_sed,
        &[
            "sed -n '1,20p' README.md",
            "sed -n '1,20p;40,60p' README.md"
        ],
        "only -n Np or N,Mp print scripts, separated by semicolons",
        Read
    ),
    spec!(
        "seq",
        parse_seq,
        &["seq 3"],
        "known formatting/separator options and one to three numbers",
        Generic
    ),
    spec!(
        "sha1sum",
        parse_checksum,
        &["sha1sum README.md"],
        "direct checksum generation from stdin or input files; check mode is rejected",
        Read
    ),
    spec!(
        "md5sum",
        parse_checksum,
        &["md5sum README.md"],
        "direct checksum generation from stdin or input files; check mode is rejected",
        Read
    ),
    spec!(
        "b2sum",
        parse_checksum,
        &["b2sum README.md"],
        "direct checksum generation from stdin or input files; check mode is rejected",
        Read
    ),
    spec!(
        "sha256sum",
        parse_checksum,
        &["sha256sum README.md"],
        "direct checksum generation from stdin or input files; check mode is rejected",
        Read
    ),
    spec!(
        "sort",
        parse_sort,
        &["sort input"],
        "known comparison/display options; output and helper options are rejected",
        Read
    ),
    spec!(
        "sqlite3",
        parse_sqlite3,
        &[
            "sqlite3 -readonly cagent.db 'SELECT name FROM sqlite_master'",
            "sqlite3 -readonly cagent.db '.tables'",
        ],
        "read-only mode with one literal database path and a parsed query-only SQL batch, including CTEs, or an allowlisted inspection dot command",
        Read
    ),
    spec!(
        "stat",
        parse_stat,
        &["stat Cargo.toml"],
        "known metadata options and input paths",
        Read
    ),
    spec!(
        "strings",
        parse_strings,
        &["strings binary"],
        "known display/encoding options and input files",
        Read
    ),
    SafeCommandSpec {
        platform: SafeCommandPlatform::Gnu,
        ..spec!(
            "tac",
            parse_tac,
            &["tac README.md"],
            "known separator options and input files",
            Read
        )
    },
    spec!(
        "tail",
        parse_tail,
        &["tail -n 20 README.md", "tail -40 README.md"],
        "known display options and input files",
        Read
    ),
    spec!(
        "tar",
        parse_tar,
        &["tar -tf archive.tar"],
        "archive listing forms only",
        List
    ),
    spec!(
        "tree",
        parse_tree,
        &["tree -L 3 crates"],
        "bounded-depth listing with display-only options and literal roots",
        List
    ),
    spec!(
        "tr",
        parse_tr,
        &["tr a-z A-Z"],
        "known translation options and one or two string operands",
        Generic
    ),
    SafeCommandSpec {
        builtin: true,
        ..spec!("true", parse_no_args, &["true"], "no arguments", Generic)
    },
    SafeCommandSpec {
        builtin: true,
        ..spec!(
            "test",
            parse_test,
            &["test -f README.md"],
            "file-existence predicate only: -f and exactly one path",
            Generic
        )
    },
    spec!(
        "uname",
        parse_uname,
        &["uname -a"],
        "known system-information options only",
        Generic
    ),
    spec!(
        "uniq",
        parse_uniq,
        &["uniq input"],
        "known display/comparison options and at most one input",
        Read
    ),
    spec!(
        "unzip",
        parse_unzip,
        &["unzip -l archive.zip"],
        "archive listing forms only",
        List
    ),
    spec!(
        "wc",
        parse_wc,
        &["wc -l README.md"],
        "known count options and input files",
        Read
    ),
    spec!(
        "which",
        parse_which,
        &["which rg"],
        "known lookup options and literal command names",
        Generic
    ),
    spec!(
        "rustc",
        parse_version,
        &["rustc --version"],
        "--version only",
        Generic
    ),
    spec!(
        "node",
        parse_version,
        &["node --version"],
        "--version only",
        Generic
    ),
    spec!(
        "python",
        parse_version,
        &["python --version"],
        "--version only",
        Generic
    ),
    spec!(
        "python3",
        parse_version,
        &["python3 --version"],
        "--version only",
        Generic
    ),
    spec!("whoami", parse_whoami, &["whoami"], "no arguments", Generic),
];

#[must_use]
pub fn safe_command_registry() -> &'static [SafeCommandSpec] {
    SAFE_COMMAND_REGISTRY
}

pub fn generally_safe_command_registry() -> &'static [SafeCommandSpec] {
    generally_safe::GENERALLY_SAFE_COMMAND_REGISTRY
}

pub(super) fn find_spec(name: &str) -> Option<&'static SafeCommandSpec> {
    SAFE_COMMAND_REGISTRY
        .iter()
        .chain(generally_safe::GENERALLY_SAFE_COMMAND_REGISTRY)
        .find(|spec| {
            spec.platform.supported() && (spec.canonical == name || spec.aliases.contains(&name))
        })
}
