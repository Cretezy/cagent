use super::{Cli, ExecCommand};

pub(super) fn run(cli: Cli, command: ExecCommand) -> Result<(), Box<dyn std::error::Error>> {
    super::run_exec_impl(cli, command)
}
