//! A private session bus and a fake Secret Service on it, for the
//! [`SecretServiceStore`](super::SecretServiceStore) tests: the parts of
//! `org.freedesktop.Secret.Service`, `Collection`, `Item` and `Prompt`
//! the store calls, with the `plain` session algorithm, one default
//! collection that may start locked, and an unlock prompt the "user"
//! accepts or dismisses.

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
    /// Whether the "user" dismisses the unlock prompt.
    pub dismiss: bool,
    /// Whether `CreateItem` fails, as a provider that refuses writes.
    pub refuse_writes: bool,
    /// Whether the `default` alias names no collection.
    pub no_default: bool,
    /// How many prompts were shown.
    pub prompts: usize,
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
        let prompt = {
            let mut state = self.state.lock().unwrap();
            if !state.locked {
                return Ok((objects, no_object()));
            }
            state.prompts += 1;
            path(format!("/org/freedesktop/secrets/prompt/{}", state.prompts))
        };
        server
            .at(
                &prompt,
                FakePrompt {
                    state: self.state.clone(),
                    objects,
                },
            )
            .await?;
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
        let item = StoredItem {
            label,
            attributes,
            value: secret.value,
        };
        let (id, new) = {
            let mut state = self.state.lock().unwrap();
            if state.locked {
                return Err(fdo::Error::AccessDenied("locked".to_owned()));
            }
            if state.refuse_writes {
                return Err(fdo::Error::Failed("writes refused".to_owned()));
            }
            let existing = state
                .items
                .iter()
                .find(|(_, stored)| replace && stored.attributes == item.attributes)
                .map(|(id, _)| *id);
            if let Some(id) = existing {
                state.items.insert(id, item);
                (id, false)
            } else {
                state.next += 1;
                let id = state.next;
                state.items.insert(id, item);
                (id, true)
            }
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
        Ok((item_path(id), no_object()))
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
        if state.locked {
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
    fn delete(&self) -> OwnedObjectPath {
        self.state.lock().unwrap().items.remove(&self.id);
        no_object()
    }
}

struct FakePrompt {
    state: Shared,
    objects: Vec<OwnedObjectPath>,
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
            if !state.dismiss {
                state.locked = false;
            }
            state.dismiss
        };
        let unlocked = if dismissed {
            Vec::new()
        } else {
            self.objects.clone()
        };
        Self::completed(&emitter, dismissed, Value::from(unlocked)).await?;
        Ok(())
    }

    #[zbus(signal)]
    async fn completed(
        emitter: &SignalEmitter<'_>,
        dismissed: bool,
        result: Value<'_>,
    ) -> zbus::Result<()>;
}
