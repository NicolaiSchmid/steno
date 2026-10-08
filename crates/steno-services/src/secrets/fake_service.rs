//! A private session bus and a fake Secret Service on it, for the
//! [`SecretServiceStore`](super::SecretServiceStore) tests: the parts of
//! `org.freedesktop.Secret.Service`, `Collection`, `Item` and `Prompt`
//! the store calls, with the `plain` session algorithm, one default
//! collection that may start locked, items locked one by one as
//! `KeePassXC` locks them, writes confirmed through a prompt as `KeePassXC`
//! confirms them, and prompts the "user" accepts, dismisses or leaves open
//! until the test answers.

// The interface macro hands every argument over by value and keeps the
// D-Bus method's arguments and `&self` whether the fake reads them or not;
// `State`'s switches are independent, one per behaviour a test asks for.
#![allow(
    clippy::needless_pass_by_value,
    clippy::struct_excessive_bools,
    clippy::unused_self,
    clippy::used_underscore_binding
)]

use std::collections::{BTreeMap, HashMap};
use std::io::BufRead as _;
use std::sync::{Arc, Mutex};

use zbus::object_server::{ObjectServer, SignalEmitter};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, fdo};

use super::secret_service::Secret;

/// `dbus-daemon` on a socket of its own in a temporary folder, ended with
/// the value. Its configuration names no service folder, so a name on it
/// is never activated: the developer's own keyring daemon cannot start
/// there, and nothing reaches the developer's session bus.
pub struct Daemon {
    child: std::process::Child,
    pub address: String,
    _folder: tempfile::TempDir,
}

impl Daemon {
    /// None when `dbus-daemon` is not installed, unless
    /// `STENO_REQUIRE_DBUS_TEST` asks for it (CI on Linux), which fails the
    /// test instead.
    pub fn start() -> Option<Self> {
        let folder = tempfile::tempdir().unwrap();
        let config = folder.path().join("bus.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:path={}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#,
                folder.path().join("bus").display()
            ),
        )
        .unwrap();
        let spawned = std::process::Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--nofork", "--nopidfile", "--print-address=1"])
            .stdout(std::process::Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                assert!(
                    std::env::var_os("STENO_REQUIRE_DBUS_TEST").is_none(),
                    "dbus-daemon did not start: {error}"
                );
                eprintln!("SKIPPED: dbus-daemon did not start: {error}");
                return None;
            }
        };
        let mut address = String::new();
        std::io::BufReader::new(child.stdout.take().expect("piped"))
            .read_line(&mut address)
            .expect("the daemon's address");
        Some(Daemon {
            child,
            address: address.trim().to_owned(),
            _folder: folder,
        })
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// An item of the fake collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredItem {
    pub label: String,
    pub attributes: BTreeMap<String, String>,
    pub value: Vec<u8>,
}

/// What the fake holds and how it behaves; the tests read and set it.
#[derive(Debug, Default)]
pub struct State {
    pub items: BTreeMap<u32, StoredItem>,
    /// The last item id handed out.
    pub next: u32,
    /// Whether the default collection is locked.
    pub locked: bool,
    /// Whether every item is locked until an unlock names it.
    pub items_locked: bool,
    /// Whether the "user" dismisses the prompts.
    pub dismiss: bool,
    /// Whether a prompt stays open until [`answer_held`] answers it.
    pub hold: bool,
    /// The prompts left open, with what each answers.
    pub held: Vec<(OwnedObjectPath, Answer)>,
    /// Whether `CreateItem` and `Delete` ask the user to confirm. A
    /// confirmed `CreateItem` answers "no object" and names the item in its
    /// prompt's `Completed`, as the specification and `KeePassXC` do; an
    /// item it replaces keeps its path.
    pub confirm_writes: bool,
    /// Whether a confirmed `CreateItem`'s `Completed` names no item.
    pub unnamed_creates: bool,
    /// Whether `CreateItem` fails, as a provider that refuses writes.
    pub refuse_writes: bool,
    /// Whether `CreateItem` keeps other bytes than it was sent.
    pub garble_writes: bool,
    /// Whether the `default` alias names no collection.
    pub no_default: bool,
    /// Whether the collection left the bus, as `KeePassXC`'s does when its
    /// database locks: its methods fail with `UnknownObject`, and so does
    /// an unlock that names it.
    pub collection_gone: bool,
    /// How many prompts were shown.
    pub prompts: usize,
    /// How many prompts were made, shown or not.
    pub prompts_made: usize,
    /// How many prompts the store dismissed.
    pub dismissals: usize,
}

impl State {
    /// The values of the items filed under `key` by the store's
    /// attributes.
    pub fn values(&self, key: &str) -> Vec<String> {
        self.items
            .values()
            .filter(|item| {
                item.attributes.get("username").map(String::as_str) == Some(key)
                    && item.attributes.get("service").map(String::as_str)
                        == Some(super::KEYRING_SERVICE)
            })
            .map(|item| String::from_utf8(item.value.clone()).unwrap())
            .collect()
    }
}

pub type Shared = Arc<Mutex<State>>;

impl State {
    /// What accepting a prompt with `answer` does.
    fn accept(&mut self, answer: &Answer) {
        if let Answer::Unlock(objects) = answer {
            self.unlock(objects);
        }
    }

    /// Unlocks what `objects` names: the collection, or the items.
    fn unlock(&mut self, objects: &[OwnedObjectPath]) {
        for object in objects {
            if object.as_str() == COLLECTION_PATH {
                self.locked = false;
            } else {
                self.items_locked = false;
            }
        }
    }

    /// Whether unlocking `objects` needs the user.
    fn needs_prompt(&self, objects: &[OwnedObjectPath]) -> bool {
        objects.iter().any(|object| {
            if object.as_str() == COLLECTION_PATH {
                self.locked
            } else {
                self.items_locked
            }
        })
    }
}

/// Answers the oldest prompt left open, as the user would.
pub async fn answer_held(fake: &Connection, state: &Shared, accept: bool) {
    let (prompt, answer) = {
        let mut state = state.lock().unwrap();
        let (prompt, answer) = state.held.remove(0);
        if accept {
            state.accept(&answer);
        }
        (prompt, answer)
    };
    let emitter = SignalEmitter::new(fake, prompt).unwrap();
    FakePrompt::completed(&emitter, !accept, answer.result(accept))
        .await
        .unwrap();
}

/// What a prompt does once accepted, and what its `Completed` carries.
#[derive(Debug, Clone)]
pub enum Answer {
    /// Unlocks these objects, and lists them.
    Unlock(Vec<OwnedObjectPath>),
    /// Names the item a `CreateItem` wrote.
    Created(OwnedObjectPath),
    /// Names nothing.
    Nothing,
}

impl Answer {
    /// The `Completed` result: what was unlocked or created, nothing once
    /// dismissed.
    fn result(&self, accepted: bool) -> Value<'static> {
        match self {
            Answer::Unlock(objects) if accepted => Value::from(objects.clone()),
            Answer::Created(item) if accepted => Value::from(item.clone()),
            _ => Value::from(Vec::<OwnedObjectPath>::new()),
        }
    }
}

/// A prompt on the fake's object server, answering `answer`.
async fn prompt_at(
    server: &ObjectServer,
    state: &Shared,
    answer: Answer,
) -> fdo::Result<OwnedObjectPath> {
    let prompt = {
        let mut state = state.lock().unwrap();
        state.prompts_made += 1;
        path(format!(
            "/org/freedesktop/secrets/prompt/{}",
            state.prompts_made
        ))
    };
    server
        .at(
            &prompt,
            FakePrompt {
                state: state.clone(),
                answer,
            },
        )
        .await?;
    Ok(prompt)
}

const SERVICE_PATH: &str = "/org/freedesktop/secrets";
const COLLECTION_PATH: &str = "/org/freedesktop/secrets/collection/test";
const SESSION_PATH: &str = "/org/freedesktop/secrets/session/1";

/// The fake on `daemon`, until the connection drops.
pub async fn serve(daemon: &Daemon, state: Shared) -> Connection {
    zbus::connection::Builder::address(daemon.address.as_str())
        .unwrap()
        .name("org.freedesktop.secrets")
        .unwrap()
        .serve_at(
            SERVICE_PATH,
            FakeService {
                state: state.clone(),
            },
        )
        .unwrap()
        .serve_at(COLLECTION_PATH, FakeCollection { state })
        .unwrap()
        .build()
        .await
        .unwrap()
}

fn path(text: String) -> OwnedObjectPath {
    OwnedObjectPath::try_from(text).unwrap()
}

fn no_object() -> OwnedObjectPath {
    path("/".to_owned())
}

struct FakeService {
    state: Shared,
}

#[zbus::interface(name = "org.freedesktop.Secret.Service")]
impl FakeService {
    fn open_session(
        &self,
        algorithm: &str,
        _input: Value<'_>,
    ) -> fdo::Result<(OwnedValue, OwnedObjectPath)> {
        if algorithm != "plain" {
            return Err(fdo::Error::NotSupported(algorithm.to_owned()));
        }
        Ok((
            Value::from("").try_into().unwrap(),
            path(SESSION_PATH.to_owned()),
        ))
    }

    fn read_alias(&self, name: &str) -> OwnedObjectPath {
        if name == "default" && !self.state.lock().unwrap().no_default {
            path(COLLECTION_PATH.to_owned())
        } else {
            no_object()
        }
    }

    async fn unlock(
        &self,
        objects: Vec<OwnedObjectPath>,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> fdo::Result<(Vec<OwnedObjectPath>, OwnedObjectPath)> {
        if self.state.lock().unwrap().collection_gone
            && objects
                .iter()
                .any(|object| object.as_str() == COLLECTION_PATH)
        {
            return Err(fdo::Error::UnknownObject(COLLECTION_PATH.to_owned()));
        }
        if !self.state.lock().unwrap().needs_prompt(&objects) {
            return Ok((objects, no_object()));
        }
        let prompt = prompt_at(server, &self.state, Answer::Unlock(objects)).await?;
        Ok((Vec::new(), prompt))
    }
}

struct FakeCollection {
    state: Shared,
}

#[zbus::interface(name = "org.freedesktop.Secret.Collection")]
impl FakeCollection {
    fn search_items(
        &self,
        attributes: HashMap<String, String>,
    ) -> fdo::Result<Vec<OwnedObjectPath>> {
        let state = self.state.lock().unwrap();
        if state.collection_gone {
            return Err(fdo::Error::UnknownObject(COLLECTION_PATH.to_owned()));
        }
        if state.locked {
            return Err(fdo::Error::AccessDenied("locked".to_owned()));
        }
        Ok(state
            .items
            .iter()
            .filter(|(_, item)| {
                attributes
                    .iter()
                    .all(|(name, value)| item.attributes.get(name) == Some(value))
            })
            .map(|(id, _)| item_path(*id))
            .collect())
    }

    async fn create_item(
        &self,
        properties: HashMap<String, OwnedValue>,
        secret: Secret,
        replace: bool,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
        let label = properties
            .get("org.freedesktop.Secret.Item.Label")
            .and_then(|value| String::try_from(value.clone()).ok())
            .unwrap_or_default();
        let attributes: BTreeMap<String, String> = properties
            .get("org.freedesktop.Secret.Item.Attributes")
            .and_then(|value| HashMap::<String, String>::try_from(value.clone()).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        assert_eq!(secret.session.as_str(), SESSION_PATH);
        let mut item = StoredItem {
            label,
            attributes,
            value: secret.value,
        };
        let (id, new, confirm) = {
            let mut state = self.state.lock().unwrap();
            if state.locked {
                return Err(fdo::Error::AccessDenied("locked".to_owned()));
            }
            if state.refuse_writes {
                return Err(fdo::Error::Failed("writes refused".to_owned()));
            }
            if state.garble_writes {
                item.value.push(b'!');
            }
            let existing = state
                .items
                .iter()
                .find(|(_, stored)| replace && stored.attributes == item.attributes)
                .map(|(id, _)| *id);
            let id = existing.unwrap_or_else(|| {
                state.next += 1;
                state.next
            });
            state.items.insert(id, item);
            let confirm = state.confirm_writes.then(|| {
                if state.unnamed_creates {
                    Answer::Nothing
                } else {
                    Answer::Created(item_path(id))
                }
            });
            (id, existing.is_none(), confirm)
        };
        if new {
            server
                .at(
                    item_path(id),
                    FakeItem {
                        state: self.state.clone(),
                        id,
                    },
                )
                .await?;
        }
        let Some(answer) = confirm else {
            return Ok((item_path(id), no_object()));
        };
        let prompt = prompt_at(server, &self.state, answer).await?;
        Ok((no_object(), prompt))
    }
}

fn item_path(id: u32) -> OwnedObjectPath {
    path(format!("{COLLECTION_PATH}/{id}"))
}

struct FakeItem {
    state: Shared,
    id: u32,
}

#[zbus::interface(name = "org.freedesktop.Secret.Item")]
impl FakeItem {
    fn get_secret(&self, session: ObjectPath<'_>) -> fdo::Result<Secret> {
        let state = self.state.lock().unwrap();
        if state.locked || state.items_locked {
            return Err(fdo::Error::AccessDenied("locked".to_owned()));
        }
        let item = state
            .items
            .get(&self.id)
            .ok_or_else(|| fdo::Error::UnknownObject("deleted".to_owned()))?;
        Ok(Secret {
            session: session.into(),
            parameters: Vec::new(),
            value: item.value.clone(),
            content_type: "text/plain".to_owned(),
        })
    }

    /// Forgets the item; the object stays on the bus, answering
    /// `GetSecret` with an error, as `SearchItems` no longer lists it.
    async fn delete(
        &self,
        #[zbus(object_server)] server: &ObjectServer,
    ) -> fdo::Result<OwnedObjectPath> {
        let confirm = {
            let mut state = self.state.lock().unwrap();
            state.items.remove(&self.id);
            state.confirm_writes
        };
        if confirm {
            return prompt_at(server, &self.state, Answer::Nothing).await;
        }
        Ok(no_object())
    }
}

struct FakePrompt {
    state: Shared,
    answer: Answer,
}

#[zbus::interface(name = "org.freedesktop.Secret.Prompt")]
impl FakePrompt {
    async fn prompt(
        &self,
        _window_id: &str,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> fdo::Result<()> {
        let dismissed = {
            let mut state = self.state.lock().unwrap();
            state.prompts += 1;
            if state.hold {
                let held = (emitter.path().to_owned().into(), self.answer.clone());
                state.held.push(held);
                return Ok(());
            }
            if !state.dismiss {
                state.accept(&self.answer);
            }
            state.dismiss
        };
        Self::completed(&emitter, dismissed, self.answer.result(!dismissed)).await?;
        Ok(())
    }

    fn dismiss(&self) {
        self.state.lock().unwrap().dismissals += 1;
    }

    #[zbus(signal)]
    async fn completed(
        emitter: &SignalEmitter<'_>,
        dismissed: bool,
        result: Value<'_>,
    ) -> zbus::Result<()>;
}
