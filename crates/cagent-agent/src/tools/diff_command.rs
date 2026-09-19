use crate::config::UiDiffMode;

/// Frontend-neutral `/diff` argument validation and default resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiffCommand {
    View(UiDiffMode),
    Clear,
}

impl DiffCommand {
    pub const USAGE: &str = "usage: /diff [conversation|git|clear]";

    pub fn parse(argument: Option<&str>, default: UiDiffMode) -> Result<Self, &'static str> {
        match argument.map(str::trim) {
            None | Some("") => Ok(Self::View(default)),
            Some("conversation") => Ok(Self::View(UiDiffMode::Conversation)),
            Some("git") => Ok(Self::View(UiDiffMode::Git)),
            Some("clear") => Ok(Self::Clear),
            Some(_) => Err(Self::USAGE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_command_defaults_overrides_and_usage() {
        for mode in [UiDiffMode::Conversation, UiDiffMode::Git] {
            assert_eq!(DiffCommand::parse(None, mode), Ok(DiffCommand::View(mode)));
            assert_eq!(
                DiffCommand::parse(Some("conversation"), mode),
                Ok(DiffCommand::View(UiDiffMode::Conversation))
            );
            assert_eq!(
                DiffCommand::parse(Some("git"), mode),
                Ok(DiffCommand::View(UiDiffMode::Git))
            );
            assert_eq!(
                DiffCommand::parse(Some("clear"), mode),
                Ok(DiffCommand::Clear)
            );
            for argument in ["jj", "git extra", "clear conversation", "unknown"] {
                assert_eq!(
                    DiffCommand::parse(Some(argument), mode),
                    Err(DiffCommand::USAGE)
                );
            }
        }
    }
}
