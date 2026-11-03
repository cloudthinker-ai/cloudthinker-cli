use cloudthinker_client::{CtError, DoctorCause, DoctorReport, diagnose};

use crate::engine::exit::{self, ExitCode};
use crate::engine::output;

pub async fn run(base_url: &str, workspace: Option<&str>, json: bool) -> ExitCode {
    output::progress(
        "Checking CLI credentials, live identity, API access, and recorded Connection status...",
    );
    let report = diagnose(base_url, workspace, env!("CARGO_PKG_VERSION")).await;
    let code = diagnostic_exit(&report);
    match output::emit_doctor(report, code, json) {
        Ok(()) => code,
        Err(error) => exit::finish_output(Err(error)),
    }
}

fn diagnostic_exit(report: &DoctorReport) -> ExitCode {
    let has = |cause| report.checks.iter().any(|check| check.cause == Some(cause));
    if has(DoctorCause::Interrupted) {
        return ExitCode::Interrupted;
    }
    let error = if has(DoctorCause::Usage) {
        CtError::Usage("diagnostic selection or request rejected".into())
    } else if has(DoctorCause::Auth) {
        CtError::Auth("diagnostic authentication or permission unavailable".into())
    } else if has(DoctorCause::Deadline) {
        CtError::Timeout("diagnostic deadline elapsed".into())
    } else if has(DoctorCause::Failure) {
        CtError::Protocol("diagnostic evidence failed or incomplete".into())
    } else {
        return ExitCode::Ok;
    };
    exit::code_for(&error)
}
