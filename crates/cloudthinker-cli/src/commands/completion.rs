use crate::engine::exit::ExitCode;
use crate::engine::output;

pub fn run(shell: clap_complete::Shell, command: &mut clap::Command) -> ExitCode {
    let mut script = Vec::new();
    clap_complete::generate(shell, command, "cloudthinker", &mut script);
    match output::print_document(&String::from_utf8_lossy(&script)) {
        Ok(()) => ExitCode::Ok,
        Err(error) => {
            output::eprintln_error(&error);
            ExitCode::JobFailed
        }
    }
}
