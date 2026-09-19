use super::super::*;

pub(super) fn parse_cagent(args: &[String]) -> Option<SafeCommandParse> {
    let accepted = match args {
        [config, get, key] if config == "config" && get == "get" => {
            !key.is_empty() && !key.split('.').any(crate::config::config_key_is_secret)
        }
        [config, view, safe] => config == "config" && view == "view" && safe == "--safe",
        [config, list] => config == "config" && list == "list",
        _ => false,
    };
    accepted.then(|| {
        SafeCommandParse::new(Some(SafeShellPresentation::Read)).path(
            "~/.config/cagent/config.toml",
            "implicit Cagent configuration",
        )
    })
}
