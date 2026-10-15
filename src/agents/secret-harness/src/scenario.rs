//! What the two pods check, as plain functions over a [`CryptoClient`], so that the same code that runs
//! in the cluster is what the tests run against a stand-in credentials agent.
//!
//! The owner stores a secret, retrieves it, replaces it, and on every later round retrieves it again to
//! show that nothing the intruder did changed it. The intruder, a different identity, tries to retrieve
//! it, tries to retrieve a name nothing holds (and must be told the same thing, so a refusal says nothing
//! about whether a secret exists), and tries to store the same name (and must be refused by policy: the name is in
//! the owner's namespace, so the refusal is the same whether or not the name is held).

use credentials_agent_common::secret::StoreRefusalReason;
use polar_agent_builder::crypto::CryptoClient;
use polar_crypto::CryptoError;

/// What the owner stores first, and then replaces it with.
pub const FIRST_VALUE: &[u8] = b"harness-value-one";
pub const SECOND_VALUE: &[u8] = b"harness-value-two";
/// What the intruder tries to put in its place.
const INTRUDER_VALUE: &[u8] = b"harness-intruder-value";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub passed: bool,
    pub detail: String,
}

fn check(name: &'static str, passed: bool, detail: impl Into<String>) -> Check {
    Check {
        name,
        passed,
        detail: detail.into(),
    }
}

/// What the answer to a retrieve was, as words, without the secret in them.
fn retrieved<T: AsRef<[u8]>>(answer: &Result<Option<T>, CryptoError>) -> String {
    match answer {
        Ok(Some(_)) => "a value".to_string(),
        Ok(None) => "nothing stored".to_string(),
        Err(e) => e.to_string(),
    }
}

async fn expect_value<C: CryptoClient>(
    c: &C,
    name: &'static str,
    secret: &str,
    expected: &[u8],
) -> Check {
    let answer = c.retrieve_secret(secret.to_string()).await;
    let passed = matches!(&answer, Ok(Some(v)) if v.as_slice() == expected);
    check(name, passed, retrieved(&answer))
}

/// The owner's checks for one round. Round 0 stores and replaces; later rounds look again.
pub async fn owner_round<C: CryptoClient>(c: &C, secret: &str, round: u64) -> Vec<Check> {
    let mut checks = Vec::new();
    if round == 0 {
        let stored = c
            .store_secret(secret.to_string(), FIRST_VALUE.to_vec())
            .await;
        checks.push(check(
            "the owner stores a secret",
            stored.is_ok(),
            format!("{stored:?}"),
        ));
        checks
            .push(expect_value(c, "the owner retrieves its own secret", secret, FIRST_VALUE).await);
        let replaced = c
            .store_secret(secret.to_string(), SECOND_VALUE.to_vec())
            .await;
        checks.push(check(
            "the owner replaces its own secret",
            replaced.is_ok(),
            format!("{replaced:?}"),
        ));
        checks.push(
            expect_value(
                c,
                "the owner retrieves the replacement",
                secret,
                SECOND_VALUE,
            )
            .await,
        );
    } else {
        checks.push(
            expect_value(
                c,
                "the owner still has its secret, unchanged",
                secret,
                SECOND_VALUE,
            )
            .await,
        );
    }
    checks
}

/// The intruder's checks for one round.
pub async fn intruder_round<C: CryptoClient>(c: &C, secret: &str) -> Vec<Check> {
    let mut checks = Vec::new();

    let theirs = c.retrieve_secret(secret.to_string()).await;
    let denied = |answer: &Result<Option<_>, CryptoError>| -> Option<String> {
        match answer {
            Err(CryptoError::RetrieveSecretDenied(why)) => Some(why.clone()),
            _ => None,
        }
    };
    let theirs_denied = denied(&theirs);
    checks.push(check(
        "the intruder is denied another identity's secret",
        theirs_denied.is_some(),
        retrieved(&theirs),
    ));

    // A name nothing holds. The answer must be the very same denial, or a denial would tell a prober
    // which names are in use.
    let absent_name = format!("{secret}-that-nobody-stored");
    let absent = c.retrieve_secret(absent_name).await;
    let absent_denied = denied(&absent);
    checks.push(check(
        "a name nothing holds is refused the same way",
        absent_denied.is_some() && absent_denied == theirs_denied,
        retrieved(&absent),
    ));

    let taken = c
        .store_secret(secret.to_string(), INTRUDER_VALUE.to_vec())
        .await;
    checks.push(check(
        "the intruder cannot take the name over",
        matches!(
            &taken,
            Err(CryptoError::Refused(StoreRefusalReason::PolicyDenied))
        ),
        format!("{taken:?}"),
    ));
    checks
}

/// Plays `role` for as long as the process lives: wait out the start delay, then one round of checks every
/// period, each check said through `say`. Nothing here ends the process; a failed check is a line in the
/// log that the results script reads, and the next round tries again.
pub async fn run<C: CryptoClient>(
    settings: &crate::settings::HarnessSettings,
    c: &C,
    say: impl Fn(&str),
) {
    use crate::settings::Role;
    let role = settings.role.name();
    tokio::time::sleep(settings.start_delay).await;
    for round in 0u64.. {
        let checks = match settings.role {
            Role::Owner => owner_round(c, &settings.secret_name, round).await,
            Role::Intruder => intruder_round(c, &settings.secret_name).await,
        };
        for check in &checks {
            say(&report(role, round, check));
        }
        tokio::time::sleep(settings.period).await;
    }
}

/// One line for the log per check, and the verdict the results script reads: the words
/// `HARNESS RESULT` first, then `pass` or `fail`.
pub fn report(role: &str, round: u64, check: &Check) -> String {
    format!(
        "HARNESS RESULT role={role} round={round} {} check=\"{}\" detail=\"{}\"",
        if check.passed { "pass" } else { "fail" },
        check.name,
        check.detail.replace('"', "'")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use polar_crypto::Zeroizing;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// A credentials agent under the first policy: an identity may retrieve what it stored, a store
    /// may only replace one's own, and the refusal of a retrieve is the same for every name.
    #[derive(Default)]
    struct Held(HashMap<String, (String, Vec<u8>)>);

    #[derive(Clone)]
    struct Fake {
        who: &'static str,
        held: Arc<Mutex<Held>>,
        /// Break the rule on purpose, to show a check notices.
        lets_anyone_retrieve: bool,
        lets_anyone_store: bool,
        says_when_absent: Option<&'static str>,
        /// What a refused store is refused for.
        refuses_with: StoreRefusalReason,
        /// A store that says it replaced the value and did not.
        ignores_replacement: bool,
    }

    impl Fake {
        fn pair() -> (Fake, Fake) {
            let held = Arc::new(Mutex::new(Held::default()));
            let make = |who| Fake {
                who,
                held: held.clone(),
                lets_anyone_retrieve: false,
                lets_anyone_store: false,
                says_when_absent: None,
                refuses_with: StoreRefusalReason::PolicyDenied,
                ignores_replacement: false,
            };
            (make("owner"), make("intruder"))
        }
    }

    #[ractor::async_trait]
    impl CryptoClient for Fake {
        async fn store_secret(&self, name: String, plaintext: Vec<u8>) -> Result<(), CryptoError> {
            let mut held = self.held.lock().unwrap();
            if let Some((by, _)) = held.0.get(&name) {
                if by != self.who && !self.lets_anyone_store {
                    return Err(CryptoError::Refused(self.refuses_with.clone()));
                }
                if self.ignores_replacement && by == self.who {
                    return Ok(());
                }
            }
            held.0.insert(name, (self.who.to_string(), plaintext));
            Ok(())
        }
        async fn retrieve_secret(
            &self,
            name: String,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, CryptoError> {
            let held = self.held.lock().unwrap();
            match held.0.get(&name) {
                Some((by, value)) if by == self.who || self.lets_anyone_retrieve => {
                    Ok(Some(Zeroizing::new(value.clone())))
                }
                None if self.says_when_absent.is_some() => Err(CryptoError::RetrieveSecretDenied(
                    self.says_when_absent.unwrap().to_string(),
                )),
                _ => Err(CryptoError::RetrieveSecretDenied(
                    "policy does not permit this identity to retrieve this secret".to_string(),
                )),
            }
        }
        async fn register_secret(
            &self,
            _: String,
            _: String,
            _: Vec<u8>,
            _: Vec<u8>,
            _: polar_crypto::registry::SecretBinding,
        ) -> Result<(), CryptoError> {
            Err(CryptoError::NoReply)
        }
        async fn load_secret(&self, _: String, _: String) -> Result<bool, CryptoError> {
            Err(CryptoError::NoReply)
        }
        async fn replace_section(
            &self,
            _: String,
            _: Vec<String>,
        ) -> Result<Vec<String>, CryptoError> {
            Err(CryptoError::NoReply)
        }
        async fn git_credential(
            &self,
            _: String,
            _: String,
        ) -> Result<polar_crypto::registry::GitCredential, CryptoError> {
            Err(CryptoError::NoReply)
        }
        async fn decrypt_own_message(&self, _: Vec<u8>) -> Result<Vec<u8>, CryptoError> {
            Err(CryptoError::NoReply)
        }
        async fn encrypt_for_recipient(
            &self,
            _: String,
            _: Vec<u8>,
        ) -> Result<Vec<u8>, CryptoError> {
            Err(CryptoError::NoReply)
        }
        async fn ensure_bootstrapped(&self) -> Result<(), CryptoError> {
            Ok(())
        }
    }

    fn all_passed(checks: &[Check]) -> bool {
        checks.iter().all(|c| c.passed)
    }

    fn failed(checks: &[Check]) -> Vec<&'static str> {
        checks
            .iter()
            .filter(|c| !c.passed)
            .map(|c| c.name)
            .collect()
    }

    #[tokio::test]
    async fn an_intruder_told_the_name_is_taken_has_learned_it_exists_and_that_is_a_failed_check() {
        // Policy answers before the ledger does, so an identity that may not store under a name is refused the
        // same way whether or not the name is held. A reply of NameTaken to the intruder would give that away.
        let (owner, mut intruder) = Fake::pair();
        owner_round(&owner, "s", 0).await;
        intruder.refuses_with = StoreRefusalReason::NameTaken;
        let checks = intruder_round(&intruder, "s").await;
        assert_eq!(
            failed(&checks),
            vec!["the intruder cannot take the name over"]
        );
    }

    #[tokio::test]
    async fn against_an_agent_that_keeps_the_rule_every_check_passes() {
        let (owner, intruder) = Fake::pair();
        let first = owner_round(&owner, "s", 0).await;
        assert_eq!(first.len(), 4);
        assert!(all_passed(&first), "{first:?}");
        let theirs = intruder_round(&intruder, "s").await;
        assert_eq!(theirs.len(), 3);
        assert!(all_passed(&theirs), "{theirs:?}");
        // After the intruder has done its worst, the owner still has the replacement.
        let later = owner_round(&owner, "s", 1).await;
        assert_eq!(later.len(), 1);
        assert!(all_passed(&later), "{later:?}");
    }

    #[tokio::test]
    async fn an_intruder_who_can_retrieve_is_caught() {
        let (owner, mut intruder) = Fake::pair();
        owner_round(&owner, "s", 0).await;
        intruder.lets_anyone_retrieve = true;
        let checks = intruder_round(&intruder, "s").await;
        assert_eq!(
            failed(&checks),
            vec![
                "the intruder is denied another identity's secret",
                "a name nothing holds is refused the same way"
            ]
        );
    }

    #[tokio::test]
    async fn an_intruder_who_can_take_the_name_is_caught_and_the_owner_notices_the_loss() {
        let (owner, mut intruder) = Fake::pair();
        owner_round(&owner, "s", 0).await;
        intruder.lets_anyone_store = true;
        let checks = intruder_round(&intruder, "s").await;
        assert_eq!(
            failed(&checks),
            vec!["the intruder cannot take the name over"]
        );
        // The secret is now the intruder's: the owner cannot retrieve it, and says so.
        let later = owner_round(&owner, "s", 1).await;
        assert_eq!(
            failed(&later),
            vec!["the owner still has its secret, unchanged"]
        );
    }

    #[tokio::test]
    async fn a_refusal_that_differs_for_an_absent_name_is_caught() {
        let (owner, mut intruder) = Fake::pair();
        owner_round(&owner, "s", 0).await;
        intruder.says_when_absent = Some("no such secret");
        let checks = intruder_round(&intruder, "s").await;
        assert_eq!(
            failed(&checks),
            vec!["a name nothing holds is refused the same way"]
        );
    }

    #[tokio::test]
    async fn a_store_refused_for_another_reason_is_not_the_name_being_taken() {
        let (owner, mut intruder) = Fake::pair();
        owner_round(&owner, "s", 0).await;
        intruder.refuses_with = StoreRefusalReason::WrongEscrowKey;
        let checks = intruder_round(&intruder, "s").await;
        assert_eq!(
            failed(&checks),
            vec!["the intruder cannot take the name over"]
        );
    }

    #[tokio::test]
    async fn an_owner_whose_replacement_does_not_take_is_caught_by_the_value_read_back() {
        let (mut owner, _intruder) = Fake::pair();
        owner.ignores_replacement = true;
        // The first store lands (nothing held yet); the replacement says Ok and changes nothing.
        let first = owner_round(&owner, "s", 0).await;
        assert_eq!(failed(&first), vec!["the owner retrieves the replacement"]);
    }

    #[tokio::test]
    async fn an_owner_who_cannot_store_or_read_back_is_caught() {
        let (owner, intruder) = Fake::pair();
        // Someone else already holds the name.
        intruder
            .store_secret("s".into(), b"theirs".to_vec())
            .await
            .unwrap();
        let first = owner_round(&owner, "s", 0).await;
        assert!(!all_passed(&first), "{first:?}");
        assert!(failed(&first).contains(&"the owner stores a secret"));
    }

    fn settings(role: crate::settings::Role, delay: u64) -> crate::settings::HarnessSettings {
        crate::settings::HarnessSettings {
            role,
            secret_name: "s".to_string(),
            period: std::time::Duration::from_secs(60),
            start_delay: std::time::Duration::from_secs(delay),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn the_intruder_waits_for_the_owner_and_both_go_on_round_after_round() {
        use crate::settings::Role;
        let (owner, intruder) = Fake::pair();
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let log = lines.clone();
        let say = move |line: &str| log.lock().unwrap().push(line.to_string());

        let owner_settings = settings(Role::Owner, 0);
        let intruder_settings = settings(Role::Intruder, 45);
        let say_owner = say.clone();
        let owner_task = tokio::spawn(async move { run(&owner_settings, &owner, say_owner).await });
        let intruder_task =
            tokio::spawn(async move { run(&intruder_settings, &intruder, say).await });

        // The owner has stored by the time the intruder first looks.
        tokio::time::sleep(std::time::Duration::from_secs(46)).await;
        {
            let lines = lines.lock().unwrap();
            let first_intruder = lines
                .iter()
                .position(|l| l.contains("role=intruder"))
                .unwrap();
            let last_owner_store = lines
                .iter()
                .rposition(|l| l.contains("the owner replaces its own secret"))
                .unwrap();
            assert!(last_owner_store < first_intruder, "{lines:#?}");
        }
        // A minute on, both have gone round again, and nothing has failed.
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let lines = lines.lock().unwrap();
        assert!(
            lines.iter().any(|l| l.contains("role=owner round=1")),
            "{lines:#?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("role=intruder round=1")),
            "{lines:#?}"
        );
        assert!(lines.iter().all(|l| l.contains(" pass ")), "{lines:#?}");
        owner_task.abort();
        intruder_task.abort();
    }

    #[test]
    fn a_result_line_says_who_which_round_and_the_verdict_and_never_the_value() {
        let pass = report("owner", 2, &check("c", true, "a value"));
        assert!(
            pass.starts_with("HARNESS RESULT role=owner round=2 pass"),
            "{pass}"
        );
        let fail = report("intruder", 0, &check("c", false, "said \"no\""));
        assert!(fail.contains(" fail "), "{fail}");
        assert!(
            fail.contains("said 'no'"),
            "quotes in a detail do not break the line: {fail}"
        );
        for value in [FIRST_VALUE, SECOND_VALUE, INTRUDER_VALUE] {
            assert!(!pass.contains(std::str::from_utf8(value).unwrap()));
        }
    }
}
