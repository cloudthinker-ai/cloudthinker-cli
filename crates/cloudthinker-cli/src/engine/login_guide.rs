//! What to do when a command finds no usable login.
//!
//! Pure so the decision is table-testable: only the caller touches the network,
//! the terminal, or the browser.

use cloudthinker_client::{CredentialProvenance, CredentialSource, CtError};

/// Printed before the browser opens, so the user knows why it did.
pub const OPENING_LOGIN: &str = "You are not logged in. Opening the CloudThinker login now.";

/// A non-interactive shell cannot answer a browser consent screen.
pub const LOG_IN_FIRST: &str =
    "Run `cloudthinker login` on a machine with a browser, then run this command again.";

/// `CLOUDTHINKER_TOKEN` outranks the stored credentials, so a login would not
/// be read even after it succeeded.
pub const REPLACE_ENV_TOKEN: &str = "The credential in CLOUDTHINKER_TOKEN is rejected. Replace it, or unset it and run `cloudthinker login`.";
pub const RENEW_STORED_LOGIN: &str =
    "Your stored login was rejected. Run `cloudthinker login` to renew it, then retry.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Not an authentication failure: report it as-is.
    Report,
    /// Authentication failed, but this process cannot fix it here.
    Tell(&'static str),
    /// Run the login flow, then retry.
    LogIn,
}

pub fn needs_login(error: &CtError) -> bool {
    match error {
        CtError::Auth(_) | CtError::ObsoleteCredentials => true,
        CtError::Api { status, .. } => *status == 401 || *status == 403,
        _ => false,
    }
}

/// What the terminal shows before the plan acts: the error the user must be
/// able to read, then the line that says what happens next. `Report` prints
/// nothing here because `exit::report` owns that error.
pub fn explain(error: &CtError, plan: Plan) -> Option<(String, &'static str)> {
    match plan {
        Plan::Report => None,
        Plan::Tell(hint) => Some((error.to_string(), hint)),
        Plan::LogIn => Some((error.to_string(), OPENING_LOGIN)),
    }
}

pub fn plan(error: &CtError, provenance: CredentialProvenance, interactive: bool) -> Plan {
    if !needs_login(error) {
        return Plan::Report;
    }
    if matches!(
        provenance,
        CredentialProvenance::Present(CredentialSource::Environment)
            | CredentialProvenance::Stale(CredentialSource::Environment)
    ) {
        return Plan::Tell(REPLACE_ENV_TOKEN);
    }
    if !interactive {
        if provenance == CredentialProvenance::Stale(CredentialSource::Stored) {
            return Plan::Tell(RENEW_STORED_LOGIN);
        }
        return Plan::Tell(LOG_IN_FIRST);
    }
    Plan::LogIn
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudthinker_client::TOKEN_ENV_VAR;

    #[test]
    fn only_an_authentication_failure_offers_a_login() {
        assert!(needs_login(&CtError::Auth("x".into())));
        assert!(needs_login(&CtError::ObsoleteCredentials));
        assert!(needs_login(&CtError::Api {
            status: 401,
            detail: None
        }));
        assert!(needs_login(&CtError::Api {
            status: 403,
            detail: None
        }));
        assert!(!needs_login(&CtError::Api {
            status: 500,
            detail: None
        }));
        assert!(!needs_login(&CtError::Transport("x".into())));
        assert!(!needs_login(&CtError::Usage("x".into())));
    }

    #[test]
    fn the_plan_reads_the_error_then_the_environment() {
        let auth = CtError::Auth("not logged in".into());

        assert_eq!(
            plan(&auth, CredentialProvenance::Missing, true),
            Plan::LogIn
        );
        assert_eq!(
            plan(&auth, CredentialProvenance::Missing, false),
            Plan::Tell(LOG_IN_FIRST)
        );
        assert_eq!(
            plan(
                &auth,
                CredentialProvenance::Present(CredentialSource::Environment),
                true
            ),
            Plan::Tell(REPLACE_ENV_TOKEN)
        );
        assert_eq!(
            plan(
                &auth,
                CredentialProvenance::Stale(CredentialSource::Stored),
                false
            ),
            Plan::Tell(RENEW_STORED_LOGIN)
        );
        assert_eq!(
            plan(
                &CtError::Transport("down".into()),
                CredentialProvenance::Missing,
                true
            ),
            Plan::Report
        );
    }

    #[test]
    fn a_login_never_hides_the_error_that_caused_it() {
        let error = CtError::Auth("keyring read: -25293; run `cloudthinker login`".into());

        let (line, next) = explain(&error, Plan::LogIn).unwrap();

        assert_eq!(line, error.to_string());
        assert_eq!(next, OPENING_LOGIN);
        assert_eq!(
            explain(&error, Plan::Tell(LOG_IN_FIRST)),
            Some((error.to_string(), LOG_IN_FIRST))
        );
        assert_eq!(
            explain(&CtError::Transport("down".into()), Plan::Report),
            None
        );
    }

    #[test]
    fn the_environment_hint_names_the_variable_it_talks_about() {
        assert!(REPLACE_ENV_TOKEN.contains(TOKEN_ENV_VAR));
    }
}
