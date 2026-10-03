//! The protocol core behind the listener: the auth gate and every route.
//! Independent of the connection and of TLS, so [`Engine::handle`] is driven
//! directly by the tests. Owned by the service; one lock around the pairing
//! session, the live receipts and the completions in flight, held for
//! synchronous sections only, never across a store, file or intake call, at
//! which the next request runs. The recording routes live in
//! `recording.rs`. Swift: `Routing/HandoverEngine.swift`,
//! `Routing/HTTPMessages.swift`.

mod recording;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bytes::Bytes;
use chrono::Duration;
use http::{HeaderMap, StatusCode};
use serde::Serialize;
use steno_core::{HandoverIntake, HandoverReceipt, HandoverState, PairedDevice, Store, store};
use tokio::sync::watch;
use uuid::Uuid;

use crate::configuration::Clock;
use crate::configuration::HandoverConfiguration;
use crate::identity::HandoverIdentity;
use crate::pairing::{DeviceTokens, PairingPayload, PairingSession};
use crate::route::{AuthRequirement, Route};
use crate::upload::{Inbox, MetadataValidation};
use crate::wire;

/// Who a request comes from, decided at the request head before the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Anonymous,
    Pairing,
    Device(PairedDevice),
}

/// The outcome of the auth gate. A rejection carries the answer the
/// connection writes before it reads the body, so every status decision of
/// the wire is the engine's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    Allowed(Principal),
    Rejected(HandoverResponse),
}

/// One complete, authenticated request handed from the connection to the
/// engine.
#[derive(Clone)]
pub struct HandoverRequest {
    pub route: Route,
    pub principal: Principal,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// The `Authorization` header carries the pairing secret or the bearer
/// token and the body of `/v1/pair` the device's request: neither belongs
/// in a log line, so the headers show as their names and the body as its
/// length.
impl std::fmt::Debug for HandoverRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandoverRequest")
            .field("route", &self.route)
            .field("principal", &self.principal)
            .field("headers", &self.headers.keys().collect::<Vec<_>>())
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl HandoverRequest {
    #[must_use]
    pub fn new(route: Route, principal: Principal) -> Self {
        HandoverRequest {
            route,
            principal,
            headers: HeaderMap::new(),
            body: Bytes::new(),
        }
    }

    #[must_use]
    pub fn with_body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    #[must_use]
    pub fn with_header(mut self, name: &'static str, value: &str) -> Self {
        if let Ok(value) = http::HeaderValue::from_str(value) {
            self.headers.insert(name, value);
        }
        self
    }

    /// The device behind a bearer route; routes with other auth never ask.
    #[must_use]
    pub fn device(&self) -> Option<&PairedDevice> {
        match &self.principal {
            Principal::Device(device) => Some(device),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoverResponse {
    pub status: StatusCode,
    pub headers: Vec<(&'static str, String)>,
    pub body: Bytes,
}

impl HandoverResponse {
    pub fn json<T: Serialize>(status: StatusCode, value: &T) -> Self {
        match serde_json::to_vec(value) {
            Ok(body) => HandoverResponse {
                status,
                headers: vec![("content-type", "application/json".to_owned())],
                body: body.into(),
            },
            Err(error) => Self::internal_error("encoding the response", &error),
        }
    }

    #[must_use]
    pub fn empty(status: StatusCode) -> Self {
        HandoverResponse {
            status,
            headers: Vec::new(),
            body: Bytes::new(),
        }
    }

    pub fn problem(status: StatusCode, message: impl Into<String>) -> Self {
        Self::json(status, &wire::Problem::new(message))
    }

    /// A 500 whose body names the step and nothing else. The error itself
    /// (a failing write, hash, promote, intake or store call) carries the
    /// inbox path, and with it the user's home directory, so it goes to the
    /// local log through `tracing` for whoever subscribes (the shell, the
    /// CLI) and never to the phone or the receipt. Swift: `HandoverLog.swift`.
    pub fn internal_error(what: &str, error: &dyn std::fmt::Display) -> Self {
        tracing::error!(target: "steno::handover", "{what} failed: {error}");
        Self::problem(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{what} failed on the computer"),
        )
    }

    /// The body decoded as `T`, for tests and clients.
    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }
}

/// The engine as the connection sees it: an auth gate that runs before the
/// body, and a request handler that runs after it.
#[steno_core::async_trait]
pub trait RequestHandling: Send + Sync {
    async fn authenticate(&self, route: Route, authorization: Option<&str>) -> AuthOutcome;
    async fn handle(&self, request: HandoverRequest) -> HandoverResponse;
}

struct State {
    pairing: Option<PairingSession>,
    /// Receipts touched since start, by recording id; what the receipt
    /// stream carries.
    active_receipts: BTreeMap<Uuid, HandoverReceipt>,
    /// Recordings whose `complete` is between the `verifying` write and the
    /// intake's answer. The verify and the admit yield, so a retried
    /// `complete` must not start a second verify or admission.
    completing: BTreeSet<Uuid>,
}

pub struct Engine {
    configuration: HandoverConfiguration,
    identity: Arc<HandoverIdentity>,
    store: Arc<Store>,
    intake: Arc<dyn HandoverIntake>,
    now: Clock,
    pub inbox: Inbox,
    receipts: watch::Sender<Vec<HandoverReceipt>>,
    state: Mutex<State>,
    /// Receipt saves run one after another, in the order they were asked
    /// for: GRDB's writer queue gives Swift that for free, the blocking
    /// pool here does not, and two saves of one receipt committing out of
    /// order would leave the older chunk set in the store.
    saves: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("inbox", &self.inbox)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// `lastSeenAt` is written at most this often per device.
    pub const LAST_SEEN_RESOLUTION_SECONDS: i64 = 60;
    /// A partial whose receipt has not moved for this long is abandoned:
    /// the phone that announced it is not coming back, and the sweep
    /// reclaims the space. The receipt stays; a late re-announce starts the
    /// upload over.
    pub const ABANDONED_AFTER_SECONDS: i64 = 14 * 24 * 60 * 60;
    /// The `failed` reason after the intake refused; the detail is logged.
    pub const INTAKE_REFUSED: &'static str = "the intake refused the file";

    /// The two answers of the gate. `unauthorized` is the phone's "the
    /// computer revoked me" signal; `pairing_rejected` a bad, used or
    /// expired secret.
    #[must_use]
    pub fn unauthorized() -> HandoverResponse {
        HandoverResponse::problem(StatusCode::UNAUTHORIZED, "unknown or revoked token")
    }

    #[must_use]
    pub fn pairing_rejected() -> HandoverResponse {
        HandoverResponse::problem(StatusCode::FORBIDDEN, "pairing secret rejected")
    }

    pub(crate) fn new(
        configuration: HandoverConfiguration,
        identity: Arc<HandoverIdentity>,
        store: Arc<Store>,
        intake: Arc<dyn HandoverIntake>,
        receipts: watch::Sender<Vec<HandoverReceipt>>,
        now: Clock,
    ) -> Self {
        Engine {
            inbox: Inbox::new(configuration.inbox_directory.clone()),
            configuration,
            identity,
            store,
            intake,
            now,
            receipts,
            state: Mutex::new(State {
                pairing: None,
                active_receipts: BTreeMap::new(),
                completing: BTreeSet::new(),
            }),
            saves: tokio::sync::Mutex::new(()),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A store call on the blocking pool.
    pub(crate) async fn with_store<T: Send + 'static>(
        &self,
        body: impl FnOnce(&Store) -> store::Result<T> + Send + 'static,
    ) -> store::Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || body(&store))
            .await
            .map_err(|error| store::StoreError::Io(std::io::Error::other(error.to_string())))?
    }

    /// On start, drop inbox files no receipt accounts for (a crash between
    /// announce and the first save, a device revoked while offline), that a
    /// completed intake left behind (it copied the file before writing
    /// `complete`), or whose receipt has not moved in
    /// [`Engine::ABANDONED_AFTER_SECONDS`]. Best effort.
    pub async fn sweep_orphans(&self) {
        let _ = self.inbox.prepare();
        let cutoff = (self.now)() - Duration::seconds(Self::ABANDONED_AFTER_SECONDS);
        for recording_id in self.inbox.recording_ids() {
            let receipt = self
                .with_store(move |store| store.handover_receipt(recording_id))
                .await
                .ok()
                .flatten();
            match receipt {
                None => self.inbox.discard(recording_id),
                Some(receipt)
                    if receipt.state.kind() == steno_core::HandoverStateKind::Complete
                        || receipt.updated_at < cutoff =>
                {
                    self.inbox.discard(recording_id);
                }
                Some(_) => {}
            }
        }
    }

    // Pairing session

    /// Opens a window and returns the payload for the QR code, replacing
    /// any open session.
    pub fn begin_pairing(&self) -> PairingPayload {
        let session = PairingSession::open(
            self.identity.mac_id(),
            &self.configuration.service_name,
            &self.identity.fingerprint(),
            self.configuration.pairing_window,
            self.now.clone(),
        );
        let payload = session.payload.clone();
        self.state().pairing = Some(session);
        payload
    }

    pub fn cancel_pairing(&self) {
        self.state().pairing = None;
    }

    #[must_use]
    pub fn pairing_is_open(&self) -> bool {
        self.state()
            .pairing
            .as_ref()
            .is_some_and(PairingSession::is_open)
    }

    /// Forgets the device and drops whatever it was uploading.
    pub async fn revoke(&self, device_id: Uuid) -> store::Result<()> {
        let mut unfinished = Vec::new();
        self.state().active_receipts.retain(|_, receipt| {
            let owned = receipt.device_id == device_id;
            if owned && receipt.state.kind() != steno_core::HandoverStateKind::Complete {
                unfinished.push(receipt.recording_id);
            }
            !owned
        });
        for recording_id in unfinished {
            self.inbox.discard(recording_id);
        }
        self.with_store(move |store| store.delete_paired_device(device_id))
            .await?;
        self.publish_receipts();
        Ok(())
    }

    // Auth gate

    /// Refreshes `last_seen_at`, at most once a minute. The gate read
    /// `device` before a yield, so the write is an `UPDATE` of the row that
    /// still holds `token_hash`: a revoke that landed in between is not
    /// undone, and the device is not re-inserted. Public for the tests,
    /// which run it against a device revoked after its read.
    pub async fn touch(&self, device: PairedDevice, token_hash: Vec<u8>) -> PairedDevice {
        let timestamp = (self.now)();
        if let Some(seen) = device.last_seen_at
            && (timestamp - seen).num_seconds() < Self::LAST_SEEN_RESOLUTION_SECONDS
        {
            return device;
        }
        let mut seen = device;
        seen.last_seen_at = Some(timestamp);
        let id = seen.id;
        let _ = self
            .with_store(move |store| store.touch_paired_device(id, &token_hash, timestamp))
            .await;
        seen
    }

    /// The credential after `<scheme> ` in an `Authorization` header, case
    /// insensitive on the scheme.
    #[must_use]
    pub fn credential<'a>(scheme: &str, authorization: Option<&'a str>) -> Option<&'a str> {
        let authorization = authorization?;
        let (presented_scheme, credential) = authorization.trim_start().split_once(' ')?;
        if !presented_scheme.eq_ignore_ascii_case(scheme) {
            return None;
        }
        let credential = credential.trim();
        (!credential.is_empty()).then_some(credential)
    }

    // Routes

    async fn pair(&self, request: &HandoverRequest) -> HandoverResponse {
        // The gate passed at the head; the window may have closed since.
        // Single use: one guard from the check to the take, so two
        // connections on two worker threads that both passed the gate
        // cannot both find the session open. Nothing in between yields; a
        // second request with the same secret that arrives while the save
        // runs finds no session and is 403. The window reopens only if the
        // save fails.
        let (session, device_id, name) = {
            let mut state = self.state();
            match &state.pairing {
                Some(session) if session.is_open() => {}
                _ => return Self::pairing_rejected(),
            }
            let body: wire::PairRequest = match serde_json::from_slice(&request.body) {
                Ok(body) => body,
                Err(error) => {
                    return HandoverResponse::problem(
                        StatusCode::BAD_REQUEST,
                        format!("PairRequest: {error}"),
                    );
                }
            };
            let name = match MetadataValidation::device_name(&body.device_name) {
                Ok(name) => name,
                Err(problem) => return HandoverResponse::problem(StatusCode::BAD_REQUEST, problem),
            };
            let session = state.pairing.take().expect("checked under this guard");
            (session, body.device_id, name.to_owned())
        };
        let token = DeviceTokens::mint();
        let timestamp = (self.now)();
        let device = PairedDevice {
            id: device_id,
            name,
            paired_at: timestamp,
            last_seen_at: Some(timestamp),
        };
        let hash = DeviceTokens::hash(&token);
        if let Err(error) = self
            .with_store(move |store| store.save_paired_device(&device, &hash))
            .await
        {
            let mut state = self.state();
            if state.pairing.is_none() {
                state.pairing = Some(session);
            }
            return HandoverResponse::internal_error("saving the device", &error);
        }
        HandoverResponse::json(
            StatusCode::OK,
            &wire::PairResponse {
                token,
                mac_id: self.identity.mac_id(),
                mac_name: self.configuration.service_name.clone(),
            },
        )
    }

    async fn unpair(&self, device: &PairedDevice) -> HandoverResponse {
        match self.revoke(device.id).await {
            Ok(()) => HandoverResponse::empty(StatusCode::NO_CONTENT),
            Err(error) => HandoverResponse::internal_error("revoking the device", &error),
        }
    }

    // Receipts

    /// Receipts touched since start, oldest first.
    #[must_use]
    pub fn receipts_snapshot(&self) -> Vec<HandoverReceipt> {
        let state = self.state();
        let mut receipts: Vec<HandoverReceipt> = state.active_receipts.values().cloned().collect();
        receipts.sort_by_key(|receipt| (receipt.created_at, receipt.recording_id));
        receipts
    }

    fn publish_receipts(&self) {
        self.receipts.send_replace(self.receipts_snapshot());
    }

    /// One state change: the state, the chunk set when given, `updated_at`,
    /// then persist. Callers that answer the phone whatever the write did
    /// ignore the result deliberately: memory already holds the change and
    /// the phone's next request re-reads.
    pub(crate) async fn transition(
        &self,
        receipt: &mut HandoverReceipt,
        state: HandoverState,
        received_chunks: Option<Vec<i64>>,
    ) -> store::Result<()> {
        receipt.state = state;
        if let Some(received_chunks) = received_chunks {
            receipt.received_chunks = received_chunks;
        }
        receipt.updated_at = (self.now)();
        self.persist(receipt).await
    }

    /// Writes the receipt and tells the observers. Memory is updated before
    /// the save runs: the phone keeps two chunks in flight, so the next
    /// request must already see this one's chunk or it would persist a stale
    /// copy over it. The save itself writes the receipt as memory holds it
    /// when the save's turn comes (`saves` is FIFO), so the last save of a
    /// burst carries every chunk of the burst.
    pub(crate) async fn persist(&self, receipt: &HandoverReceipt) -> store::Result<()> {
        self.state()
            .active_receipts
            .insert(receipt.recording_id, receipt.clone());
        let result = {
            let _turn = self.saves.lock().await;
            let saved = self
                .active_receipt(receipt.recording_id)
                .unwrap_or_else(|| receipt.clone());
            self.with_store(move |store| store.save_handover_receipt(&saved))
                .await
        };
        self.publish_receipts();
        result
    }

    /// The receipt from memory or the store. Another request may have loaded
    /// and advanced it while the store read ran; memory wins then.
    pub(crate) async fn receipt(&self, recording_id: Uuid) -> Option<HandoverReceipt> {
        if let Some(active) = self.state().active_receipts.get(&recording_id) {
            return Some(active.clone());
        }
        let stored = self
            .with_store(move |store| store.handover_receipt(recording_id))
            .await
            .ok()
            .flatten()?;
        let mut state = self.state();
        if let Some(active) = state.active_receipts.get(&recording_id) {
            return Some(active.clone());
        }
        state.active_receipts.insert(recording_id, stored.clone());
        Some(stored)
    }

    /// The receipt as memory holds it now, for the re-read after a yield.
    pub(crate) fn active_receipt(&self, recording_id: Uuid) -> Option<HandoverReceipt> {
        self.state().active_receipts.get(&recording_id).cloned()
    }

    /// Replaces `receipt` with what memory holds after a yield, when another
    /// request advanced it meanwhile.
    pub(crate) fn refresh(&self, receipt: &mut HandoverReceipt) {
        if let Some(current) = self.active_receipt(receipt.recording_id) {
            *receipt = current;
        }
    }

    /// The receipt when it belongs to the requesting device.
    pub(crate) async fn owned_receipt(
        &self,
        recording_id: Uuid,
        device: &PairedDevice,
    ) -> Option<HandoverReceipt> {
        self.receipt(recording_id)
            .await
            .filter(|receipt| receipt.device_id == device.id)
    }

    /// Marks `recording_id` as completing; `None` when it already is.
    pub(crate) fn begin_completing(&self, recording_id: Uuid) -> Option<Completing<'_>> {
        // The guard is built only once the lock is released: its `Drop`
        // takes the same lock, and `then_some` would build (and drop) it
        // eagerly.
        let inserted = self.state().completing.insert(recording_id);
        inserted.then(|| Completing {
            engine: self,
            recording_id,
        })
    }
}

/// The `completing` mark, removed when the `complete` call returns.
pub(crate) struct Completing<'a> {
    engine: &'a Engine,
    recording_id: Uuid,
}

impl Drop for Completing<'_> {
    fn drop(&mut self) {
        self.engine.state().completing.remove(&self.recording_id);
    }
}

#[steno_core::async_trait]
impl RequestHandling for Engine {
    /// Runs at the request head, before the body; a rejection carries the
    /// answer the connection writes.
    async fn authenticate(&self, route: Route, authorization: Option<&str>) -> AuthOutcome {
        match route.auth() {
            AuthRequirement::None => AuthOutcome::Allowed(Principal::Anonymous),
            AuthRequirement::Pairing => {
                let matched = Self::credential("Pairing", authorization).is_some_and(|secret| {
                    self.state()
                        .pairing
                        .as_ref()
                        .is_some_and(|session| session.matches(secret))
                });
                if matched {
                    AuthOutcome::Allowed(Principal::Pairing)
                } else {
                    AuthOutcome::Rejected(Self::pairing_rejected())
                }
            }
            AuthRequirement::Bearer => {
                let Some(token) = Self::credential("Bearer", authorization) else {
                    return AuthOutcome::Rejected(Self::unauthorized());
                };
                let hash = DeviceTokens::hash(token);
                let lookup = hash.clone();
                let device = self
                    .with_store(move |store| store.paired_device_for_token_hash(&lookup))
                    .await
                    .ok()
                    .flatten();
                match device {
                    Some(device) => {
                        AuthOutcome::Allowed(Principal::Device(self.touch(device, hash).await))
                    }
                    None => AuthOutcome::Rejected(Self::unauthorized()),
                }
            }
        }
    }

    async fn handle(&self, request: HandoverRequest) -> HandoverResponse {
        // Every bearer route passed the gate with a device principal.
        match (request.route, request.device()) {
            (Route::Hello, _) => {
                HandoverResponse::json(StatusCode::OK, &wire::Hello::new(self.identity.mac_id()))
            }
            (Route::Pair, _) => self.pair(&request).await,
            (_, None) => Self::unauthorized(),
            (Route::Unpair, Some(device)) => self.unpair(device).await,
            (Route::Announce(recording_id), Some(device)) => {
                self.announce(recording_id, device, &request.body).await
            }
            (Route::Status(recording_id), Some(device)) => self.status(recording_id, device).await,
            (Route::Chunk(recording_id, index), Some(device)) => {
                self.receive_chunk(recording_id, index, device, &request)
                    .await
            }
            (Route::Complete(recording_id), Some(device)) => {
                self.complete(recording_id, device).await
            }
        }
    }
}
