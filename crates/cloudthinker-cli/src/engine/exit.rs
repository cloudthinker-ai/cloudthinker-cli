//! The single source of truth for process exit codes.
//!
//! No command calls `process::exit`; each returns an `ExitCode` and `main`
//! converts it once. Error → code mapping also lives here so the contract is
//! auditable in one place.

use cloudthinker_client::CtError;

/// Stable CLI exit codes (documented in `wiki/concepts/cli`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    /// Success.
    Ok = 0,
    /// The job itself failed (a FAILED run, a 404 run, exhausted transport retries).
    JobFailed = 1,
    /// Bad usage or a server-side validation/secret-gate rejection (422).
    Usage = 2,
    /// Authentication missing/expired; the user must log in.
    Auth = 3,
    /// A client deadline elapsed (login wait or `chat --timeout`).
    Timeout = 4,
    /// The run paused for human approval in the browser.
    ApprovalRequired = 5,
}

impl ExitCode {
    pub fn process(self) -> std::process::ExitCode {
        std::process::ExitCode::from(self as u8)
    }
}

/// Map a client error to its exit code — pure, so it is table-testable.
pub fn code_for(err: &CtError) -> ExitCode {
    match err {
        CtError::Auth(_) | CtError::ObsoleteCredentials => ExitCode::Auth,
        CtError::Usage(_) => ExitCode::Usage,
        CtError::Timeout(_) => ExitCode::Timeout,
        CtError::LoginDenied => ExitCode::JobFailed,
        CtError::Api { status, .. } => match status {
            401 | 403 => ExitCode::Auth,
            422 => ExitCode::Usage,
            _ => ExitCode::JobFailed,
        },
        CtError::Transport(_)
        | CtError::Store(_)
        | CtError::Login(_)
        | CtError::Logout(_)
        | CtError::Protocol(_) => ExitCode::JobFailed,
    }
}

/// Print `err` to stderr (colored when appropriate) and return its exit code.
pub fn report(err: &CtError) -> ExitCode {
    crate::engine::output::eprintln_error(&err.to_string());
    code_for(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The stable exit-code contract, as a table. Changing a row is a breaking
    // change to any script that keys off `cloudthinker`'s exit status.
    #[test]
    fn exit_code_values_are_stable() {
        assert_eq!(ExitCode::Ok as u8, 0);
        assert_eq!(ExitCode::JobFailed as u8, 1);
        assert_eq!(ExitCode::Usage as u8, 2);
        assert_eq!(ExitCode::Auth as u8, 3);
        assert_eq!(ExitCode::Timeout as u8, 4);
        assert_eq!(ExitCode::ApprovalRequired as u8, 5);
    }

    #[test]
    fn error_to_code_mapping_table() {
        let cases = [
            (CtError::Auth("x".into()), ExitCode::Auth),
            (CtError::ObsoleteCredentials, ExitCode::Auth),
            (CtError::Usage("x".into()), ExitCode::Usage),
            (CtError::Timeout("x".into()), ExitCode::Timeout),
            (CtError::LoginDenied, ExitCode::JobFailed),
            (CtError::Transport("x".into()), ExitCode::JobFailed),
            (CtError::Store("x".into()), ExitCode::JobFailed),
            (CtError::Protocol("x".into()), ExitCode::JobFailed),
            (CtError::Logout("x".into()), ExitCode::JobFailed),
            (
                CtError::Api {
                    status: 401,
                    detail: None,
                },
                ExitCode::Auth,
            ),
            (
                CtError::Api {
                    status: 403,
                    detail: None,
                },
                ExitCode::Auth,
            ),
            (
                CtError::Api {
                    status: 422,
                    detail: None,
                },
                ExitCode::Usage,
            ),
            (
                CtError::Api {
                    status: 404,
                    detail: None,
                },
                ExitCode::JobFailed,
            ),
            (
                CtError::Api {
                    status: 500,
                    detail: None,
                },
                ExitCode::JobFailed,
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(code_for(&err), expected, "mapping for {err:?}");
        }
    }
}
