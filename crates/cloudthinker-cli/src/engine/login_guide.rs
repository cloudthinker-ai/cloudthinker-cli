//! What to do when a command finds no usable login.
//!
//! Pure so the decision is table-testable: only the caller touches the network,
//! the terminal, or the browser.

use cloudthinker_client::{CredentialProvenance, CredentialSource, CtError, login_command};

/// Printed before the browser opens, so the user knows why it did.
pub const OPENING_LOGIN: &str = "You are not logged in. Opening the CloudThinker login now.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    /// A non-interactive shell cannot answer a browser consent screen.
    LogInFirst,
    /// `CLOUDTHINKER_TOKEN` outranks the stored credentials, so a login would
    /// not be read even after it succeeded.
    ReplaceEnvToken,
    RenewStoredLogin,
}

impl Hint {
    pub fn text(self, base_url: &str) -> String {
        let login = login_command(base_url);
        match self {
            Hint::LogInFirst => {
                format!("Run `{login}` on a machine with a browser, then run this command again.")
            }
            Hint::ReplaceEnvToken => format!(
                "The credential in CLOUDTHINKER_TOKEN is rejected. Replace it, or unset it and run `{login}`."
            ),
            Hint::RenewStoredLogin => {
                format!("Your stored login was rejected. Run `{login}` to renew it, then retry.")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Not an authentication failure: report it as-is.
    Report,
    /// Authentication failed, but this process cannot fix it here.
    Tell(Hint),
    /// Run the login flow, then retry.
    LogIn,
}

pub fn needs_login(error: &CtError) -> bool {
    match error {
        CtError::Auth(_) | CtError::ObsoleteCredentials { .. } => true,
        CtError::Api { status, .. } => *status == 401,
        _ => false,
    }
}

/// What the terminal shows before the plan acts: the error the user must be
/// able to read, then the line that says what happens next. `Report` prints
/// nothing here because `exit::report` owns that error.
pub fn explain(error: &CtError, plan: Plan, base_url: &str) -> Option<(String, String)> {
    match plan {
        Plan::Report => None,
        Plan::Tell(hint) => Some((error.to_string(), hint.text(base_url))),
        Plan::LogIn => Some((error.to_string(), OPENING_LOGIN.to_string())),
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
        return Plan::Tell(Hint::ReplaceEnvToken);
    }
    if !interactive {
        if provenance == CredentialProvenance::Stale(CredentialSource::Stored) {
            return Plan::Tell(Hint::RenewStoredLogin);
        }
        return Plan::Tell(Hint::LogInFirst);
    }
    Plan::LogIn
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloudthinker_client::TOKEN_ENV_VAR;

    const DEFAULT: &str = "https://app.cloudthinker.io";

    #[test]
    fn only_an_authentication_failure_offers_a_login() {
        assert!(needs_login(&CtError::Auth("x".into())));
        assert!(needs_login(&CtError::ObsoleteCredentials {
            login: "cloudthinker login".into()
        }));
        assert!(needs_login(&CtError::Api {
            status: 401,
            detail: None
        }));
        assert!(!needs_login(&CtError::Api {
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
            Plan::Tell(Hint::LogInFirst)
        );
        assert_eq!(
            plan(
                &auth,
                CredentialProvenance::Present(CredentialSource::Environment),
                true
            ),
            Plan::Tell(Hint::ReplaceEnvToken)
        );
        assert_eq!(
            plan(
                &auth,
                CredentialProvenance::Stale(CredentialSource::Stored),
                false
            ),
            Plan::Tell(Hint::RenewStoredLogin)
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

        let (line, next) = explain(&error, Plan::LogIn, DEFAULT).unwrap();

        assert_eq!(line, error.to_string());
        assert_eq!(next, OPENING_LOGIN);
        assert_eq!(
            explain(&error, Plan::Tell(Hint::LogInFirst), DEFAULT),
            Some((error.to_string(), Hint::LogInFirst.text(DEFAULT)))
        );
        assert_eq!(
            explain(&CtError::Transport("down".into()), Plan::Report, DEFAULT),
            None
        );
    }

    #[test]
    fn the_environment_hint_names_the_variable_it_talks_about() {
        assert!(Hint::ReplaceEnvToken.text(DEFAULT).contains(TOKEN_ENV_VAR));
    }

    #[test]
    fn every_hint_names_the_host_it_was_given() {
        for hint in [
            Hint::LogInFirst,
            Hint::ReplaceEnvToken,
            Hint::RenewStoredLogin,
        ] {
            assert!(hint.text(DEFAULT).contains("`cloudthinker login`"));
            assert!(
                hint.text("https://dev.cloudthinker.io")
                    .contains("`cloudthinker login --url https://dev.cloudthinker.io`"),
                "{hint:?}"
            );
        }
    }
}
