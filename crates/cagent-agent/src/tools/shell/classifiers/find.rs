use super::super::*;

pub(super) fn parse_find(args: &[String]) -> Option<SafeCommandParse> {
    let mut index = 0;
    while index < args.len() && matches!(args[index].as_str(), "-H" | "-P") {
        index += 1;
    }
    let mut roots = Vec::new();
    while index < args.len()
        && !args[index].starts_with('-')
        && !matches!(args[index].as_str(), "!" | "(" | ")")
    {
        roots.push(args[index].clone());
        index += 1;
    }
    let mut search = false;
    let mut plain_path_output = true;
    while index < args.len() {
        let token = args[index].as_str();
        match token {
            "!"
            | "("
            | ")"
            | "-a"
            | "-and"
            | "-o"
            | "-or"
            | "-not"
            | "-print"
            | "-print0"
            | "-ls"
            | "-prune"
            | "-true"
            | "-false"
            | "-readable"
            | "-writable"
            | "-executable"
            | "-empty"
            | "-nouser"
            | "-nogroup"
            | "-daystart"
            | "-ignore_readdir_race"
            | "-depth"
            | "-d"
            | "-xdev"
            | "-mount"
            | "-quit"
            | "-type"
            | "-xtype" => {
                if matches!(token, "-print0" | "-ls") {
                    plain_path_output = false;
                }
                if matches!(token, "-type" | "-xtype") {
                    index += 1;
                    args.get(index)?;
                    search = true;
                }
            }
            "-name" | "-iname" | "-path" | "-ipath" | "-wholename" | "-iwholename" | "-regex"
            | "-iregex" | "-perm" | "-user" | "-group" | "-uid" | "-gid" | "-size" | "-links"
            | "-inum" | "-maxdepth" | "-mindepth" | "-mtime" | "-mmin" | "-atime" | "-amin"
            | "-ctime" | "-cmin" | "-printf" => {
                if token == "-printf" {
                    plain_path_output = false;
                }
                index += 1;
                args.get(index)?;
                search = true;
            }
            _ => return None,
        }
        index += 1;
    }
    if roots.is_empty() {
        roots.push(".".into());
    }
    let mut result = SafeCommandParse::new(Some(if search {
        SafeShellPresentation::Search
    } else {
        SafeShellPresentation::List
    }));
    for root in roots {
        result = result.path(root, "search root");
    }
    if plain_path_output {
        result.path_list_output = Some(SafePathListOutput::Lines);
    }
    Some(result)
}
