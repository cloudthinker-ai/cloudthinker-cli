//! What to do when a command finds no usable login.
//!
//! Pure so the decision is table-testable: only the caller touches the network,
//! the terminal, or the browser.

use cloudthinker_client::CtError;

/// Printed before the browser opens, so the user knows why it did.
pub const OPENING_LOGIN: &str = "You are not logged in. Opening the CloudThinker login now.";

/// A non-interactive shell cannot answer a browser consent screen.
pub const LOG_IN_FIRST: &str =
    "Run `cloudthinker login` on a machine with a browser, then run this command again.";

/// `CLOUDTHINKER_TOKEN` outranks the stored credentials, so a login would not
/// be read even after it succeeded.
pub const REPLACE_ENV_TOKEN: &str = "The credential in CLOUDTHINKER_TOKEN is rejected. Replace it, or unset it and run `cloudthinker login`.";

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

pub fn plan(error: &CtError, env_token_is_set: bool, interactive: bool) -> Plan {
    if !needs_login(error) {
        return Plan::Report;
    }
    if env_token_is_set {
        return Plan::Tell(REPLACE_ENV_TOKEN);
    }
    if !interactive {
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

        assert_eq!(plan(&auth, false, true), Plan::LogIn);
        assert_eq!(plan(&auth, false, false), Plan::Tell(LOG_IN_FIRST));
        assert_eq!(plan(&auth, true, true), Plan::Tell(REPLACE_ENV_TOKEN));
        assert_eq!(
            plan(&CtError::Transport("down".into()), false, true),
            Plan::Report
        );
    }

    #[test]
    fn the_environment_hint_names_the_variable_it_talks_about() {
        assert!(REPLACE_ENV_TOKEN.contains(TOKEN_ENV_VAR));
    }
}
