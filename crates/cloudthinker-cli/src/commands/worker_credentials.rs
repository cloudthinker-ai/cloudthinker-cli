use cloudthinker_client::auth::worker_store::{WORKER_TOKEN_ENV, WorkerCredential, WorkerStore};
use cloudthinker_client::{CtError, CtResult, worker_api::WorkerClient, worker_types as api};
use uuid::Uuid;

use super::StartArgs;

pub(super) const UNNAMED_OUTPOST: &str = "outpost";

#[derive(Debug, PartialEq, Eq)]
enum CredentialSource {
    Register(String),
    Stored(String),
    Env { target_id: Uuid, token: String },
}

fn credential_source(
    outpost: Option<&str>,
    register: Option<String>,
    stored_credential: bool,
    env_token: Option<String>,
) -> CtResult<CredentialSource> {
    if stored_credential && register.is_some() {
        return Err(CtError::Usage(
            "--stored-credential cannot be combined with --register".into(),
        ));
    }
    if let Some(reference) = register {
        return Ok(CredentialSource::Register(reference));
    }
    if stored_credential {
        return Ok(CredentialSource::Stored(
            outpost
                .ok_or_else(|| CtError::Usage("--stored-credential requires --outpost".into()))?
                .to_owned(),
        ));
    }
    if let Some(token) = env_token {
        let target_id = outpost
            .and_then(|selector| Uuid::parse_str(selector).ok())
            .ok_or_else(|| {
                CtError::Usage("CLOUDTHINKER_WORKER_TOKEN requires an outpost id".into())
            })?;
        if token.trim().is_empty() {
            return Err(CtError::Auth("worker credential is empty".into()));
        }
        return Ok(CredentialSource::Env { target_id, token });
    }
    Ok(CredentialSource::Stored(
        outpost
            .ok_or_else(|| CtError::Usage("provide --outpost or --register".into()))?
            .to_owned(),
    ))
}

fn registered_credential(
    outpost: Option<&str>,
    boot: api::WorkerBootstrap,
) -> CtResult<WorkerCredential> {
    if outpost
        .and_then(|selector| Uuid::parse_str(selector).ok())
        .is_some_and(|id| id != boot.target_id)
    {
        return Err(CtError::Protocol(
            "worker registration target mismatch".into(),
        ));
    }
    Ok(WorkerCredential {
        target_id: boot.target_id,
        name: outpost.unwrap_or(UNNAMED_OUTPOST).to_owned(),
        token: boot.credential,
    })
}

pub(super) async fn resolve_credential(
    base_url: &str,
    store: &WorkerStore,
    args: &StartArgs,
) -> CtResult<(WorkerCredential, bool)> {
    let source = credential_source(
        args.outpost.as_deref(),
        args.register.clone(),
        args.stored_credential,
        std::env::var(WORKER_TOKEN_ENV).ok(),
    )?;
    match source {
        CredentialSource::Register(reference) => {
            let boot = WorkerClient::exchange(base_url, &reference, super::CLI_VERSION).await?;
            let credential = registered_credential(args.outpost.as_deref(), boot)?;
            store.save(&credential)?;
            Ok((credential, true))
        }
        CredentialSource::Stored(selector) => Ok((store.load(&selector)?, true)),
        CredentialSource::Env { target_id, token } => Ok((
            WorkerCredential {
                target_id,
                name: UNNAMED_OUTPOST.into(),
                token,
            },
            false,
        )),
    }
}

#[cfg(unix)]
pub(super) struct Rotator {
    pub(super) store: WorkerStore,
    pub(super) client: WorkerClient,
    pub(super) executor: std::sync::Arc<crate::worker::executor::WorkdirExecutor>,
    pub(super) target_id: Uuid,
    pub(super) name: String,
    pub(super) token: std::sync::Mutex<String>,
}

#[cfg(unix)]
impl Rotator {
    pub(super) async fn rotate(&self) -> CtResult<()> {
        let current = self
            .token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Ok(stored) = self.store.load(&self.target_id.to_string())
            && stored.token != current
        {
            return self.switch(stored.token);
        }
        let boot = self.client.rotate().await?;
        if boot.target_id != self.target_id {
            return Err(CtError::Protocol("worker rotation target mismatch".into()));
        }
        let saved = self.store.save(&WorkerCredential {
            target_id: self.target_id,
            name: self.name.clone(),
            token: boot.credential.clone(),
        });
        self.switch(boot.credential)?;
        if saved.is_err() {
            crate::engine::output::warn(
                "Could not save the rotated Worker credential. After a restart, enroll this Worker again with the command from the Workers page.",
            );
        }
        Ok(())
    }

    fn switch(&self, token: String) -> CtResult<()> {
        self.client.adopt(&token)?;
        self.executor.add_secret(token.clone());
        *self
            .token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = token;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bootstrap(target_id: Uuid) -> api::WorkerBootstrap {
        api::WorkerBootstrap {
            credential: "worker-token".into(),
            credential_generation: 1,
            expires_at: chrono::Utc::now(),
            target_id,
        }
    }

    #[test]
    fn ca_wo_06_register_wins_over_a_stored_credential() {
        assert_eq!(
            credential_source(Some("build"), Some("one-time".into()), false, None).unwrap(),
            CredentialSource::Register("one-time".into())
        );
    }

    #[test]
    fn ca_wo_06_register_and_stored_credential_together_are_a_usage_error() {
        let error = credential_source(Some("build"), Some("one-time".into()), true, None)
            .expect_err("the two credential sources conflict");
        assert!(
            matches!(&error, CtError::Usage(message) if message.contains("--stored-credential")),
            "got {error:?}"
        );
    }

    #[test]
    fn ca_wo_04_stored_credential_needs_an_outpost_selector() {
        assert_eq!(
            credential_source(Some("build"), None, true, Some("env-token".into())).unwrap(),
            CredentialSource::Stored("build".into())
        );
        assert!(matches!(
            credential_source(None, None, true, None),
            Err(CtError::Usage(_))
        ));
    }

    #[test]
    fn ca_wo_05_environment_token_requires_an_outpost_uuid() {
        let id = Uuid::from_u128(7);
        assert_eq!(
            credential_source(Some(&id.to_string()), None, false, Some("env-token".into()))
                .unwrap(),
            CredentialSource::Env {
                target_id: id,
                token: "env-token".into()
            }
        );
        assert!(matches!(
            credential_source(Some("build"), None, false, Some("env-token".into())),
            Err(CtError::Usage(_))
        ));
    }

    #[test]
    fn ca_wo_05_an_empty_environment_token_is_an_auth_error() {
        let id = Uuid::from_u128(7);
        let error = credential_source(Some(&id.to_string()), None, false, Some("  ".into()))
            .expect_err("an empty credential cannot authenticate");
        assert!(matches!(error, CtError::Auth(_)), "got {error:?}");
    }

    #[test]
    fn ca_wo_04_no_source_at_all_asks_for_an_outpost_or_a_registration() {
        let error = credential_source(None, None, false, None)
            .expect_err("the worker has no credential to use");
        assert!(
            matches!(&error, CtError::Usage(message) if message == "provide --outpost or --register"),
            "got {error:?}"
        );
    }

    #[test]
    fn ca_wo_06_registration_keeps_the_exchanged_target() {
        let id = Uuid::from_u128(9);
        let credential = registered_credential(Some(&id.to_string()), bootstrap(id)).unwrap();
        assert_eq!(credential.target_id, id);
        assert_eq!(credential.token, "worker-token");
        assert_eq!(
            registered_credential(None, bootstrap(id)).unwrap().name,
            "outpost"
        );
    }

    #[test]
    fn ca_wo_06_registration_for_another_outpost_is_a_protocol_error() {
        let Err(error) = registered_credential(
            Some(&Uuid::from_u128(9).to_string()),
            bootstrap(Uuid::from_u128(10)),
        ) else {
            panic!("the exchanged target must match --outpost");
        };
        assert!(matches!(error, CtError::Protocol(_)), "got {error:?}");
    }
}
