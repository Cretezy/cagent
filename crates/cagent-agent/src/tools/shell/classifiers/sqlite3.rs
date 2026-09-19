use super::super::*;
use sqlparser::{
    ast::{Query, SetExpr, Statement},
    dialect::SQLiteDialect,
    parser::Parser,
};

pub(super) fn parse_sqlite3(args: &[String]) -> Option<SafeCommandParse> {
    let mut readonly = false;
    let mut positional = Vec::new();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-readonly" => readonly = true,
            "-header" | "-column" | "-box" | "-csv" | "-json" | "-line" | "-list" | "-table"
            | "-bail" => {}
            "-separator" | "-nullvalue" => {
                index += 1;
                if args.get(index).is_none_or(String::is_empty) {
                    return None;
                }
            }
            value if !value.starts_with('-') => positional.push(value.to_owned()),
            _ => return None,
        }
        index += 1;
    }
    if !readonly || positional.len() != 2 {
        return None;
    }
    let [database, query] = positional.as_slice() else {
        return None;
    };
    if database.is_empty() || !is_read_only_sql_input(query) {
        return None;
    }
    let mut parsed = SafeCommandParse::new(Some(SafeShellPresentation::Read))
        .path(database.clone(), "SQLite database");
    parsed.detail = Some(
        SafePresentationDetail::new("Query sqlite")
            .arguments([query.clone()], false)
            .relation("in", [database.clone()], true),
    );
    Some(parsed)
}

fn is_read_only_sql_input(input: &str) -> bool {
    is_read_only_sql_query(input) || is_read_only_dot_command(input)
}

fn is_read_only_sql_query(query: &str) -> bool {
    let query = query.trim();
    if query.is_empty()
        || query.contains("--")
        || query.contains("/*")
        || !has_no_empty_sql_statements(query)
    {
        return false;
    }

    Parser::parse_sql(&SQLiteDialect {}, query).is_ok_and(|statements| {
        !statements.is_empty()
            && statements.iter().all(|statement| match statement {
                Statement::Query(query) => is_query_only(query),
                _ => false,
            })
    })
}

fn is_query_only(query: &Query) -> bool {
    query
        .with
        .as_ref()
        .is_none_or(|with| with.cte_tables.iter().all(|cte| is_query_only(&cte.query)))
        && is_query_body_only(&query.body)
}

fn is_query_body_only(body: &SetExpr) -> bool {
    match body {
        SetExpr::Select(_) | SetExpr::Values(_) | SetExpr::Table(_) => true,
        SetExpr::Query(query) => is_query_only(query),
        SetExpr::SetOperation { left, right, .. } => {
            is_query_body_only(left) && is_query_body_only(right)
        }
        SetExpr::Insert(_) | SetExpr::Update(_) | SetExpr::Delete(_) | SetExpr::Merge(_) => false,
    }
}

/// Reject empty statements without mistaking semicolons inside SQLite string or identifier quotes
/// for separators. A single trailing separator is allowed.
fn has_no_empty_sql_statements(query: &str) -> bool {
    let mut chars = query.chars().peekable();
    let mut quote_end = None;
    let mut statement_has_content = false;
    let mut ended_with_separator = false;

    while let Some(character) = chars.next() {
        if let Some(end) = quote_end {
            if character == end {
                if end != ']' && chars.peek() == Some(&end) {
                    chars.next();
                } else {
                    quote_end = None;
                }
            }
            statement_has_content = true;
            continue;
        }

        match character {
            '\'' | '"' | '`' => {
                quote_end = Some(character);
                statement_has_content = true;
                ended_with_separator = false;
            }
            '[' => {
                quote_end = Some(']');
                statement_has_content = true;
                ended_with_separator = false;
            }
            ';' => {
                if !statement_has_content {
                    return false;
                }
                statement_has_content = false;
                ended_with_separator = true;
            }
            character if !character.is_whitespace() => {
                statement_has_content = true;
                ended_with_separator = false;
            }
            _ => {}
        }
    }

    statement_has_content || ended_with_separator
}

fn is_read_only_dot_command(command: &str) -> bool {
    if command.is_empty()
        || command.contains(['\n', '\r', ';'])
        || command.chars().any(char::is_control)
    {
        return false;
    }
    let words = command.split_ascii_whitespace().collect::<Vec<_>>();
    let Some((name, args)) = words.split_first() else {
        return false;
    };
    match *name {
        ".tables" | ".indexes" | ".dbinfo" => {
            args.len() <= 1 && args.iter().all(|arg| is_dot_positional(arg))
        }
        ".databases" => args.is_empty(),
        ".schema" => dot_options_and_positionals(args, &["--indent", "--nosys"], 1),
        ".fullschema" => dot_options_and_positionals(args, &["--indent"], 0),
        ".dump" => dot_options_and_positionals(
            args,
            &["--data-only", "--newlines", "--nosys", "--preserve-rowids"],
            usize::MAX,
        ),
        _ => false,
    }
}

fn dot_options_and_positionals(args: &[&str], options: &[&str], max_positionals: usize) -> bool {
    let mut seen_options = Vec::new();
    let mut positionals = 0;
    for arg in args {
        if arg.starts_with('-') {
            if positionals != 0 || !options.contains(arg) || seen_options.contains(arg) {
                return false;
            }
            seen_options.push(*arg);
        } else {
            if !is_dot_positional(arg) {
                return false;
            }
            positionals += 1;
            if positionals > max_positionals {
                return false;
            }
        }
    }
    true
}

fn is_dot_positional(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('.')
        && !value.starts_with('-')
        && !value.contains(['\'', '"', '`', '\\'])
        && !value.chars().any(char::is_control)
}
