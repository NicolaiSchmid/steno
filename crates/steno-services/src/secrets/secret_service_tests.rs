use std::collections::BTreeMap;
use std::path::Path;

use super::super::fake_service::{Daemon, Shared, State, answer_held, serve};
use super::*;

fn file_at(path: &Path, environment: &[(&str, &str)]) -> FileSecretStore {
    FileSecretStore::new(path, entries(environment))
}

fn write_file(path: &Path, entries: &[(&str, &str)]) {
    let map: BTreeMap<_, _> = entries.iter().copied().collect();
    std::fs::write(path, serde_json::to_vec(&map).unwrap()).unwrap();
}

fn identity() -> SecretKey {
    HandoverIdentity::secret_key()
}

fn minted(name: &str) -> HandoverIdentity {
    HandoverIdentity::mint(name, chrono::Utc::now()).unwrap()
}

fn fingerprint(identity: &HandoverIdentity) -> String {
    steno_handover::identity::hex(&identity.fingerprint())
}

/// A store over a fresh fake: the daemon, the fake's connection (kept for
/// its lifetime), the fake's state and the secrets file's folder.
struct Setup {
    daemon: Daemon,
    fake: Option<Connection>,
    state: Shared,
    folder: tempfile::TempDir,
}

impl Setup {
    /// None when `dbus-daemon` is missing (the test skips).
    async fn new(provider: bool, state: State) -> Option<Self> {
        let daemon = Daemon::start()?;
        let state = Shared::new(std::sync::Mutex::new(state));
        let fake = if provider {
            Some(serve(&daemon, state.clone()).await)
        } else {
            None
        };
        Some(Setup {
            daemon,
            fake,
            state,
            folder: tempfile::tempdir().unwrap(),
        })
    }

    fn path(&self) -> std::path::PathBuf {
        self.folder.path().join("secrets.json")
    }

    /// One launch's store.
    fn store(&self, environment: &[(&str, &str)]) -> SecretServiceStore {
        self.store_with_timeout(environment, PROMPT_TIMEOUT)
    }

    fn store_with_timeout(
        &self,
        environment: &[(&str, &str)],
        prompt_timeout: Duration,
    ) -> SecretServiceStore {
        SecretServiceStore::on_bus(
            file_at(&self.path(), environment),
            self.record(),
            &self.daemon.address,
            prompt_timeout,
        )
    }

    /// The handover identity's fingerprint record beside the file.
    fn record(&self) -> FingerprintFile {
        FingerprintFile::in_support_directory(self.folder.path())
    }

    /// A launch, once its choice is made.
    async fn launch(&self) -> SecretServiceStore {
        let store = self.store(&[]);
        store.chosen().await;
        store
    }

    fn values(&self, key: &str) -> Vec<String> {
        self.state().values(key)
    }

    fn contents(&self) -> Contents {
        file_at(&self.path(), &[]).read().unwrap()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

fn entries(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn moved(pairs: &[(&str, &str)]) -> Contents {
    Contents {
        moved: true,
        entries: entries(pairs),
    }
}

fn chose_service(store: &SecretServiceStore) -> bool {
    matches!(store.shared.backend.get(), Some(Backend::Service(_)))
}

fn chose_file(store: &SecretServiceStore) -> bool {
    matches!(store.shared.backend.get(), Some(Backend::File))
}

/// Waits until the fake holds an open prompt.
async fn until_held(setup: &Setup) {
    for _ in 0..500 {
        if !setup.state().held.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no prompt was shown");
}

#[tokio::test]
async fn secrets_are_read_written_and_removed_through_the_service() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let store = setup.store(&[]);
    let key = SecretKey::llm_api_key();
    assert_eq!(store.secret(&key).await.unwrap(), None);
    assert!(chose_service(&store));
    store.set_secret(&key, Some("sk-1")).await.unwrap();
    store.set_secret(&key, Some("sk-2")).await.unwrap();
    assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-2"));
    {
        let state = setup.state();
        let items: Vec<_> = state.items.values().collect();
        assert_eq!(items.len(), 1, "a second write replaces the item");
        assert_eq!(items[0].label, "Steno llm-api-key");
        assert_eq!(
            items[0].attributes,
            entries(&[("service", KEYRING_SERVICE), ("username", "llm-api-key")])
        );
    }
    store.set_secret(&identity(), Some("pem")).await.unwrap();
    store.set_secret(&key, None).await.unwrap();
    assert_eq!(store.secret(&key).await.unwrap(), None);
    assert_eq!(setup.values("handover-identity"), ["pem"]);
    store.set_secret(&identity(), Some("")).await.unwrap();
    assert!(setup.state().items.is_empty());
    assert_eq!(
        setup.contents(),
        moved(&[]),
        "the file holds the marker only"
    );
    assert_eq!(setup.state().prompts, 0);
}

#[tokio::test]
async fn without_a_provider_every_secret_stays_in_the_file() {
    let Some(setup) = Setup::new(false, State::default()).await else {
        return;
    };
    write_file(&setup.path(), &[("llm-api-key", "sk-file")]);
    let store = setup.store(&[]);
    let key = SecretKey::llm_api_key();
    assert_eq!(
        store.secret(&key).await.unwrap().as_deref(),
        Some("sk-file")
    );
    assert!(chose_file(&store));
    store.set_secret(&key, Some("sk-2")).await.unwrap();
    assert_eq!(
        setup.contents().entries,
        entries(&[("llm-api-key", "sk-2")])
    );
    assert!(!setup.contents().moved);
}

#[tokio::test]
async fn the_files_entries_move_and_leave_the_file_one_launch_later() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    write_file(
        &setup.path(),
        &[("llm-api-key", "sk-file"), ("handover-identity", "pem")],
    );
    let first = setup.store(&[]);
    assert_eq!(
        first.secret(&identity()).await.unwrap().as_deref(),
        Some("pem")
    );
    assert_eq!(setup.values("llm-api-key"), ["sk-file"]);
    assert_eq!(setup.values("handover-identity"), ["pem"]);
    assert_eq!(
        setup.contents(),
        moved(&[("llm-api-key", "sk-file"), ("handover-identity", "pem")]),
        "the launch that wrote the items keeps the file's copies"
    );
    drop(first);

    let second = setup.launch().await;
    assert!(chose_service(&second));
    assert_eq!(
        setup.contents(),
        moved(&[]),
        "a later launch read them back"
    );
    assert_eq!(setup.values("handover-identity"), ["pem"]);
}

#[tokio::test]
async fn a_value_the_provider_lost_is_written_again_before_the_file_lets_go() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    write_file(&setup.path(), &[("handover-identity", "pem")]);
    drop(setup.launch().await);
    // The provider never saved the item to disk before it stopped.
    setup.state().items.clear();
    drop(setup.launch().await);
    assert_eq!(setup.values("handover-identity"), ["pem"]);
    assert_eq!(setup.contents(), moved(&[("handover-identity", "pem")]));
    drop(setup.launch().await);
    assert_eq!(setup.contents(), moved(&[]));
}

#[tokio::test]
async fn before_the_move_the_api_key_takes_the_files_newer_value() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let key = SecretKey::llm_api_key();
    setup
        .launch()
        .await
        .set_secret(&key, Some("sk-old"))
        .await
        .unwrap();
    // A file without the marker, as a build from before the move left it.
    write_file(&setup.path(), &[("llm-api-key", "sk-new")]);
    let store = setup.store(&[]);
    assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-new"));
    assert_eq!(setup.values("llm-api-key"), ["sk-new"]);
}

#[tokio::test]
async fn a_removal_in_a_run_without_the_keyring_removes_the_item_at_the_move() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let key = SecretKey::llm_api_key();
    setup
        .launch()
        .await
        .set_secret(&key, Some("sk-1"))
        .await
        .unwrap();
    write_file(&setup.path(), &[]);
    setup.state().no_default = true;
    let without = setup.launch().await;
    assert!(chose_file(&without));
    without.set_secret(&key, None).await.unwrap();
    assert_eq!(without.secret(&key).await.unwrap(), None);
    assert_eq!(setup.contents().entries, entries(&[("llm-api-key", "")]));
    drop(without);

    setup.state().no_default = false;
    let store = setup.launch().await;
    assert!(chose_service(&store));
    assert_eq!(store.secret(&key).await.unwrap(), None);
    assert!(
        setup.state().items.is_empty(),
        "the removed key stays removed"
    );
    drop(store);
    drop(setup.launch().await);
    assert_eq!(setup.contents(), moved(&[]), "and leaves the file");
}

#[tokio::test]
async fn the_service_keeps_its_handover_identity_and_the_file_keeps_its_own() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    setup
        .launch()
        .await
        .set_secret(&identity(), Some("pem-service"))
        .await
        .unwrap();
    // A file that never saw the marker, with another identity.
    write_file(&setup.path(), &[("handover-identity", "pem-file")]);
    for _ in 0..2 {
        let store = setup.launch().await;
        assert_eq!(
            store.secret(&identity()).await.unwrap().as_deref(),
            Some("pem-service")
        );
    }
    assert_eq!(setup.values("handover-identity"), ["pem-service"]);
    assert_eq!(
        setup.contents(),
        moved(&[("handover-identity", "pem-file")])
    );
}

/// The service's identity (a keyring synced from another computer) is not
/// the one this computer's phones paired with: with nothing recorded, the
/// first move records the file's, so the handover finds the service's
/// replaced instead of adopting it. A recorded fingerprint stays.
#[tokio::test]
async fn an_identity_the_service_already_holds_is_not_adopted_over_the_files() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let (ours, theirs) = (minted("ours"), minted("theirs"));
    setup
        .launch()
        .await
        .set_secret(&identity(), Some(&theirs.to_pem().unwrap()))
        .await
        .unwrap();
    write_file(
        &setup.path(),
        &[("handover-identity", &ours.to_pem().unwrap())],
    );
    assert_eq!(setup.record().recorded().unwrap(), None);
    let store = setup.launch().await;
    assert!(chose_service(&store));
    assert_eq!(setup.record().recorded().unwrap(), Some(fingerprint(&ours)));
    let database = steno_core::Store::in_memory().unwrap();
    let load = HandoverIdentity::load_or_create(
        &store,
        &setup.record(),
        &database,
        "x",
        chrono::Utc::now(),
    )
    .await;
    assert!(
        matches!(
            load,
            Err(steno_handover::IdentityError::Unavailable(
                steno_handover::Unavailability::Replaced
            ))
        ),
        "{load:?}"
    );

    setup.record().record("recorded-before").unwrap();
    write_file(
        &setup.path(),
        &[("handover-identity", &ours.to_pem().unwrap())],
    );
    drop(setup.launch().await);
    assert_eq!(
        setup.record().recorded().unwrap().as_deref(),
        Some("recorded-before")
    );
}

#[tokio::test]
async fn after_the_move_the_service_wins_and_an_older_file_key_goes() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let key = SecretKey::llm_api_key();
    write_file(&setup.path(), &[("llm-api-key", "sk-1")]);
    setup
        .launch()
        .await
        .set_secret(&key, Some("sk-2"))
        .await
        .unwrap();
    let store = setup.launch().await;
    assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-2"));
    assert_eq!(setup.contents(), moved(&[]));
}

/// The loss this guards against: a launch that cannot open the keyring
/// read the identity as missing, minted a new one into the file, and the
/// next launch moved it over the phones' pinned one.
#[tokio::test]
async fn after_the_move_a_launch_without_the_keyring_fails_instead_of_reading_none() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    write_file(&setup.path(), &[("handover-identity", "pem")]);
    drop(setup.launch().await);
    drop(setup.launch().await);
    assert_eq!(setup.contents(), moved(&[]));

    {
        let mut state = setup.state();
        state.locked = true;
        state.dismiss = true;
    }
    let locked = setup.launch().await;
    let read = locked.secret(&identity()).await.unwrap_err();
    assert!(chose_file(&locked));
    assert!(
        read.to_string().contains("could not open when it started"),
        "{read}"
    );
    assert!(locked.set_secret(&identity(), Some("pem-2")).await.is_err());
    assert_eq!(setup.contents(), moved(&[]), "nothing was written");
    drop(locked);

    setup.state().dismiss = false;
    let unlocked = setup.launch().await;
    assert_eq!(
        unlocked.secret(&identity()).await.unwrap().as_deref(),
        Some("pem")
    );
    assert_eq!(setup.values("handover-identity"), ["pem"]);
}

#[tokio::test]
async fn a_value_that_does_not_read_back_keeps_the_file_unmarked() {
    let state = State {
        garble_writes: true,
        ..State::default()
    };
    let Some(setup) = Setup::new(true, state).await else {
        return;
    };
    write_file(&setup.path(), &[("llm-api-key", "sk-file")]);
    let store = setup.store(&[]);
    let key = SecretKey::llm_api_key();
    assert_eq!(
        store.secret(&key).await.unwrap().as_deref(),
        Some("sk-file")
    );
    assert!(chose_file(&store));
    assert_eq!(
        setup.contents(),
        Contents {
            moved: false,
            entries: entries(&[("llm-api-key", "sk-file")]),
        }
    );
}

#[tokio::test]
async fn without_a_default_collection_every_secret_stays_in_the_file() {
    let state = State {
        no_default: true,
        ..State::default()
    };
    let Some(setup) = Setup::new(true, state).await else {
        return;
    };
    let store = setup.store(&[]);
    let key = SecretKey::llm_api_key();
    store.set_secret(&key, Some("sk-1")).await.unwrap();
    assert!(chose_file(&store));
    assert_eq!(
        setup.contents().entries,
        entries(&[("llm-api-key", "sk-1")])
    );
    assert!(!setup.contents().moved);
    assert!(setup.state().items.is_empty());
}

#[tokio::test]
async fn a_refused_move_keeps_the_file_for_every_secret() {
    let state = State {
        refuse_writes: true,
        ..State::default()
    };
    let Some(setup) = Setup::new(true, state).await else {
        return;
    };
    write_file(&setup.path(), &[("llm-api-key", "sk-file")]);
    let store = setup.store(&[]);
    let key = SecretKey::llm_api_key();
    assert_eq!(
        store.secret(&key).await.unwrap().as_deref(),
        Some("sk-file")
    );
    assert!(chose_file(&store));
    store
        .set_secret(&SecretKey::from("other"), Some("x"))
        .await
        .unwrap();
    assert_eq!(
        setup.contents().entries,
        entries(&[("llm-api-key", "sk-file"), ("other", "x")])
    );
}

#[tokio::test]
async fn a_locked_keyring_is_unlocked_through_one_prompt_and_reads_never_ask() {
    let state = State {
        locked: true,
        ..State::default()
    };
    let Some(setup) = Setup::new(true, state).await else {
        return;
    };
    let store = setup.launch().await;
    let key = SecretKey::llm_api_key();
    store.set_secret(&key, Some("sk-1")).await.unwrap();
    assert!(chose_service(&store));
    assert_eq!(setup.state().prompts, 1);

    // Locked again while the app runs: a read fails and shows nothing.
    setup.state().locked = true;
    let read = store.secret(&key).await.unwrap_err();
    assert_eq!(read.to_string(), KeyringUnavailable::Locked.to_string());
    assert_eq!(setup.state().prompts, 1);
    assert_eq!(setup.state().dismissals, 1);
    // A write is the user's own action and asks.
    store.set_secret(&key, Some("sk-2")).await.unwrap();
    assert_eq!(setup.state().prompts, 2);
    assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-2"));
}

#[tokio::test]
async fn a_dismissed_unlock_keeps_every_secret_in_the_file_for_the_run() {
    let state = State {
        locked: true,
        dismiss: true,
        ..State::default()
    };
    let Some(setup) = Setup::new(true, state).await else {
        return;
    };
    write_file(&setup.path(), &[("llm-api-key", "sk-file")]);
    let store = setup.launch().await;
    let key = SecretKey::llm_api_key();
    assert_eq!(
        store.secret(&key).await.unwrap().as_deref(),
        Some("sk-file")
    );
    assert!(chose_file(&store));
    store.set_secret(&key, Some("sk-2")).await.unwrap();
    assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-2"));
    assert_eq!(setup.state().prompts, 1, "asked once, never again");
    assert!(setup.state().items.is_empty());
    assert_eq!(
        setup.contents().entries,
        entries(&[("llm-api-key", "sk-2")])
    );
}

#[tokio::test]
async fn while_the_keyring_asks_calls_fail_at_once_and_the_store_opens_after_the_answer() {
    let state = State {
        locked: true,
        hold: true,
        ..State::default()
    };
    let Some(setup) = Setup::new(true, state).await else {
        return;
    };
    write_file(&setup.path(), &[("llm-api-key", "sk-file")]);
    let store = setup.store(&[]);
    let unlocked = store.unlocked_after_prompt();
    until_held(&setup).await;
    let key = SecretKey::llm_api_key();
    let read = tokio::time::timeout(Duration::from_secs(1), store.secret(&key))
        .await
        .expect("a read does not wait on the user");
    assert_eq!(
        read.unwrap_err().to_string(),
        KeyringUnavailable::Unlocking.to_string()
    );
    assert!(store.set_secret(&key, Some("sk-2")).await.is_err());

    setup.state().hold = false;
    answer_held(setup.fake.as_ref().unwrap(), &setup.state, true).await;
    tokio::time::timeout(Duration::from_secs(5), unlocked)
        .await
        .expect("the store says it opened");
    assert!(chose_service(&store));
    assert_eq!(
        store.secret(&key).await.unwrap().as_deref(),
        Some("sk-file")
    );
}

#[tokio::test]
async fn the_store_opening_without_asking_never_says_it_unlocked() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let store = setup.launch().await;
    let unlocked = store.unlocked_after_prompt();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), unlocked)
            .await
            .is_err()
    );
}

/// A prompt that turned no call away leaves nothing to read again.
#[tokio::test]
async fn a_choice_that_asked_but_turned_no_call_away_never_says_to_read_again() {
    let state = State {
        locked: true,
        ..State::default()
    };
    let Some(setup) = Setup::new(true, state).await else {
        return;
    };
    let store = setup.store(&[]);
    let unlocked = store.unlocked_after_prompt();
    store.chosen().await;
    assert!(chose_service(&store));
    assert_eq!(setup.state().prompts, 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), unlocked)
            .await
            .is_err()
    );
}

/// The prompt closed after the timeout and the file chosen: a read turned
/// away while it was open is made again, and the file answers it.
#[tokio::test]
async fn an_unanswered_prompt_is_dismissed_and_the_file_answers_the_read_it_turned_away() {
    let state = State {
        locked: true,
        hold: true,
        ..State::default()
    };
    let Some(setup) = Setup::new(true, state).await else {
        return;
    };
    write_file(&setup.path(), &[("llm-api-key", "sk-file")]);
    let store = setup.store_with_timeout(&[], Duration::from_millis(200));
    let unlocked = store.unlocked_after_prompt();
    until_held(&setup).await;
    let key = SecretKey::llm_api_key();
    assert!(
        store.secret(&key).await.is_err(),
        "turned away while asking"
    );
    store.chosen().await;
    assert!(chose_file(&store));
    assert_eq!(setup.state().dismissals, 1, "the prompt was closed");
    tokio::time::timeout(Duration::from_secs(2), unlocked)
        .await
        .expect("the store says to read again");
    assert_eq!(
        store.secret(&key).await.unwrap().as_deref(),
        Some("sk-file")
    );
}

#[tokio::test]
async fn items_locked_one_by_one_are_unlocked_by_the_choice_and_never_by_a_read() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let key = SecretKey::llm_api_key();
    setup
        .launch()
        .await
        .set_secret(&key, Some("sk-1"))
        .await
        .unwrap();
    setup.state().items_locked = true;
    let store = setup.launch().await;
    assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-1"));
    assert_eq!(setup.state().prompts, 1, "the choice asked once");

    setup.state().items_locked = true;
    assert!(store.secret(&key).await.is_err());
    assert_eq!(setup.state().prompts, 1);
    assert_eq!(setup.state().dismissals, 1);
}

#[tokio::test]
async fn of_two_items_for_one_key_the_read_takes_the_first_and_a_write_leaves_one() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let store = setup.launch().await;
    let key = SecretKey::llm_api_key();
    store.set_secret(&key, Some("first")).await.unwrap();
    // Another tool filed a second item under the same attributes.
    store
        .set_secret(&SecretKey::from("other"), Some("second"))
        .await
        .unwrap();
    {
        let mut state = setup.state();
        let first = state.items[&1].attributes.clone();
        state.items.get_mut(&2).unwrap().attributes = first;
    }
    let store = setup.launch().await;
    for _ in 0..3 {
        assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("first"));
    }
    store.set_secret(&key, Some("third")).await.unwrap();
    assert_eq!(setup.values("llm-api-key"), ["third"]);
}

#[tokio::test]
async fn writes_the_provider_confirms_complete_through_its_prompt() {
    let state = State {
        confirm_writes: true,
        ..State::default()
    };
    let Some(setup) = Setup::new(true, state).await else {
        return;
    };
    let store = setup.launch().await;
    let key = SecretKey::llm_api_key();
    store.set_secret(&key, Some("sk-1")).await.unwrap();
    store.set_secret(&key, None).await.unwrap();
    assert_eq!(setup.state().prompts, 2);
    assert!(setup.state().items.is_empty());
}

/// `KeePassXC` answers a `CreateItem` that replaces an item with "no
/// object" and a prompt naming the item it updated in place: that item is
/// the one written, never an extra to delete.
#[tokio::test]
async fn a_confirmed_overwrite_keeps_the_item_it_updated() {
    let state = State {
        confirm_writes: true,
        ..State::default()
    };
    let Some(setup) = Setup::new(true, state).await else {
        return;
    };
    let store = setup.launch().await;
    let key = SecretKey::llm_api_key();
    for value in ["sk-1", "sk-2", "sk-3"] {
        store.set_secret(&key, Some(value)).await.unwrap();
        assert_eq!(setup.values("llm-api-key"), [value]);
        assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some(value));
    }
}

/// The handover identity written again over itself (`HandoverIdentity::store`
/// over an identity, as an import replaces one) keeps the new identity.
#[tokio::test]
async fn a_confirmed_identity_store_over_an_identity_keeps_the_new_one() {
    let state = State {
        confirm_writes: true,
        ..State::default()
    };
    let Some(setup) = Setup::new(true, state).await else {
        return;
    };
    let store = setup.launch().await;
    let record = setup.record();
    let (first, second) = (minted("first"), minted("second"));
    first.store(&store, &record).await.unwrap();
    second.store(&store, &record).await.unwrap();
    assert_eq!(setup.values("handover-identity").len(), 1);
    let database = steno_core::Store::in_memory().unwrap();
    let loaded =
        HandoverIdentity::load_or_create(&store, &record, &database, "x", chrono::Utc::now())
            .await
            .unwrap();
    assert_eq!(fingerprint(&loaded), fingerprint(&second));
}

/// The first move over a key the service already holds, through confirmed
/// writes: the file's value replaces it, and the move goes through.
#[tokio::test]
async fn a_confirmed_move_over_a_key_the_service_holds_keeps_the_files_value() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let key = SecretKey::llm_api_key();
    setup
        .launch()
        .await
        .set_secret(&key, Some("sk-old"))
        .await
        .unwrap();
    write_file(&setup.path(), &[("llm-api-key", "sk-new")]);
    setup.state().confirm_writes = true;
    let store = setup.launch().await;
    assert!(chose_service(&store), "the move went through");
    assert_eq!(setup.values("llm-api-key"), ["sk-new"]);
    assert!(setup.contents().moved);
}

/// Of two items for one key, a confirmed create deletes the other once its
/// prompt names the item it wrote; one that names no item may have written
/// either, so it deletes none.
#[tokio::test]
async fn a_confirmed_create_deletes_the_other_item_only_once_it_names_its_own() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let store = setup.launch().await;
    let key = SecretKey::llm_api_key();
    store.set_secret(&key, Some("first")).await.unwrap();
    store
        .set_secret(&SecretKey::from("other"), Some("second"))
        .await
        .unwrap();
    {
        let mut state = setup.state();
        let first = state.items[&1].attributes.clone();
        state.items.get_mut(&2).unwrap().attributes = first;
        state.confirm_writes = true;
        state.unnamed_creates = true;
    }
    store.set_secret(&key, Some("third")).await.unwrap();
    assert_eq!(setup.values("llm-api-key"), ["third", "second"]);
    assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("third"));
    setup.state().unnamed_creates = false;
    store.set_secret(&key, Some("fourth")).await.unwrap();
    assert_eq!(setup.values("llm-api-key"), ["fourth"]);
}

/// A run that could not open the keyring after the move says the secrets
/// are in the keyring, not in a file.
#[tokio::test]
async fn after_the_move_a_run_on_the_file_says_the_keyring() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    write_file(&setup.path(), &[("llm-api-key", "sk-file")]);
    let first = setup.launch().await;
    assert_eq!(first.place(), Some(SecretPlace::Keyring));
    drop(first);
    setup.state().no_default = true;
    let without = setup.launch().await;
    assert!(chose_file(&without));
    assert_eq!(without.place(), Some(SecretPlace::Keyring));

    let unmoved = Setup::new(false, State::default()).await.unwrap();
    let store = unmoved.launch().await;
    assert_eq!(store.place(), Some(SecretPlace::File));
}

#[tokio::test]
async fn the_environment_wins_over_the_service() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let key = SecretKey::llm_api_key();
    setup
        .launch()
        .await
        .set_secret(&key, Some("sk-service"))
        .await
        .unwrap();
    let store = setup.store(&[("STENO_LLM_API_KEY", "sk-env")]);
    assert_eq!(store.secret(&key).await.unwrap().as_deref(), Some("sk-env"));
    assert_eq!(setup.values("llm-api-key"), ["sk-service"]);
}

/// Omarchy's GNOME Keyring keeps an unencrypted file with one line per
/// secret; a line break in a value would make it reject the whole file.
#[tokio::test]
async fn a_value_with_line_breaks_is_stored_on_one_line_and_reads_back_whole() {
    let Some(setup) = Setup::new(true, State::default()).await else {
        return;
    };
    let pem = "-----BEGIN CERTIFICATE-----\nAAAA\r\nBBBB\n-----END CERTIFICATE-----\n";
    write_file(&setup.path(), &[("handover-identity", pem)]);
    let store = setup.launch().await;
    assert!(chose_service(&store), "the move read it back whole");
    let stored = setup.values("handover-identity").remove(0);
    assert!(!stored.contains(['\n', '\r']), "{stored}");
    assert!(stored.starts_with(ONE_LINE_PREFIX));
    assert_eq!(
        store.secret(&identity()).await.unwrap().as_deref(),
        Some(pem)
    );
    let key = SecretKey::llm_api_key();
    store.set_secret(&key, Some("sk-one-line")).await.unwrap();
    assert_eq!(
        setup.values("llm-api-key"),
        ["sk-one-line"],
        "stored as it is"
    );
}

#[test]
fn a_value_with_any_line_break_is_encoded_and_a_damaged_encoding_is_an_error() {
    let key = SecretKey::llm_api_key();
    for value in ["a\rb", "a\nb", "a\r\nb"] {
        let stored = one_line(value);
        assert!(stored.starts_with(ONE_LINE_PREFIX), "{value:?}");
        assert_eq!(from_one_line(&key, stored).unwrap(), value);
    }
    assert_eq!(one_line("sk-1"), "sk-1");
    assert!(matches!(
        from_one_line(&key, format!("{ONE_LINE_PREFIX}not base64!")),
        Err(ServiceError::NotText(_))
    ));
}
