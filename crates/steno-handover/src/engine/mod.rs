//! The protocol core behind the listener: the auth gate and every route.
//! Independent of the connection and of TLS, so [`Engine::handle`] is driven
//! directly by the tests. Owned by the service; one lock around the pairing
//! session, the live receipts, the completions in flight, the revoked
//! devices and the line of store writes, held for synchronous sections
//! only, never across a store, file or intake call, so another request runs
//! while one awaits. Even so, the store commits the engine's writes in the
//! order they were asked for (`Engine::in_order`). A second lock orders the
//! creation of a recording's inbox files with the discards that leave
//! another device's upload alone (`Engine::discard_own`); it is held across
//! those file calls only, never across a yield, and taken before the state
//! lock, never inside it. The recording routes live in `recording.rs`.
//! Swift: `Routing/HandoverEngine.swift`, `Routing/HTTPMessages.swift`.

mod recording;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use bytes::Bytes;
use chrono::Duration;
use http::{HeaderMap, StatusCode};
use serde::Serialize;
use steno_core::{
    HandoverIntake, HandoverReceipt, HandoverState, HandoverStateKind, PairedDevice,
    RecordingMetadata, Store, store,
};
use tokio::sync::{oneshot, watch};
use uuid::Uuid;

use crate::configuration::{Clock, HandoverConfiguration};
use crate::identity::HandoverIdentity;
use crate::pairing::{DeviceTokens, PairingPayload, PairingSession};
use crate::route::{AuthRequirement, Route};
use crate::upload::{Inbox, MetadataValidation};
use crate::wire;

/// Who a request comes from, decided at the request head before the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Anonymous,
    /// The secret matched the pairing window with this number (the engine
    /// bumps it on every open and every cancel); `/v1/pair` pairs only
    /// while that window is still the open one.
    Pairing(u64),
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
/// token, so headers show as their names; the body (audio, or JSON) shows
/// as its length.
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

#[derive(Clone, PartialEq, Eq)]
pub struct HandoverResponse {
    pub status: StatusCode,
    pub headers: Vec<(&'static str, String)>,
    pub body: Bytes,
}

/// The body of a `/v1/pair` answer carries the bearer token, so the body
/// shows as its length.
impl std::fmt::Debug for HandoverResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandoverResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body_len", &self.body.len())
            .finish()
    }
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

#[derive(Default)]
struct State {
    pairing: Option<PairingSession>,
    /// The pairing window's number, bumped on every open and every cancel.
    /// A head authorised against an earlier window whose body arrives after
    /// a cancel and a reopen must not pair against the new one, and a
    /// failed save gives its session back only if the number is unchanged.
    window: u64,
    /// Receipts touched since start, by recording id; what the receipt
    /// stream carries.
    active_receipts: BTreeMap<Uuid, HandoverReceipt>,
    /// Recordings whose `complete` is between the `verifying` write and the
    /// intake's answer. The verify and the admit yield, so a retried
    /// `complete` must not start a second verify or admission.
    completing: BTreeSet<Uuid>,
    /// Devices revoked since start and not paired again: their receipts
    /// stay out of `active_receipts` and the stream, also when a request
    /// that read one before the revoke writes it back, and their recording
    /// routes answer 401 before they read, also while the device is still
    /// in the store (a failed delete, or one not committed yet).
    revoked: BTreeSet<Uuid>,
    /// Revokes per device since start. A pairing never resets the count,
    /// so a `complete` from before the revoke still sees it after the phone
    /// pairs again under the same device id.
    revocations: BTreeMap<Uuid, u64>,
    /// Closes once the store write asked for last has returned
    /// ([`Engine::in_order`]).
    last_write: Option<oneshot::Receiver<()>>,
}

/// A store write's place in line ([`State::next_write`]): `previous`
/// closes once the write asked for before it has returned, and dropping
/// `done` once this write has returned lets the next one go.
#[must_use = "a place dropped before `Engine::in_order` lets the write behind it go at once"]
struct InOrder {
    previous: Option<oneshot::Receiver<()>>,
    done: oneshot::Sender<()>,
}

impl State {
    /// The device's revoke count, 0 when it was not revoked since start.
    fn revocation_count(&self, device_id: Uuid) -> u64 {
        self.revocations.get(&device_id).copied().unwrap_or(0)
    }

    /// Whether memory holds a receipt of `recording_id` that belongs to a
    /// device other than `device_id`: another phone announced the same
    /// recording id after this device's receipt left memory (a revoke, a
    /// refusal). A discard or forget by recording id on behalf of
    /// `device_id` leaves that phone's upload alone then.
    fn holds_another_devices(&self, recording_id: Uuid, device_id: Uuid) -> bool {
        self.active_receipts
            .get(&recording_id)
            .is_some_and(|receipt| receipt.device_id != device_id)
    }

    /// Keeps `receipt` as the live copy, unless its device was revoked.
    fn remember(&mut self, receipt: &HandoverReceipt) {
        if !self.revoked.contains(&receipt.device_id) {
            self.active_receipts
                .insert(receipt.recording_id, receipt.clone());
        }
    }

    /// The next place in the line of store writes ([`Engine::in_order`]).
    /// Taken under the same guard as the memory the write stands for (the
    /// revoke count it bumps or reads, the receipt it saves), so the store
    /// commits the writes in the order memory changed. The place must reach
    /// [`Engine::in_order`] before any yield or panic: dropped on the way,
    /// it lets the write behind it go at once, before the writes ahead of
    /// it have returned.
    fn next_write(&mut self) -> InOrder {
        let (done, last) = oneshot::channel();
        InOrder {
            previous: self.last_write.replace(last),
            done,
        }
    }
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
    /// Held while a recording's inbox files are created
    /// ([`Engine::open_files`], [`Engine::reopen_missing_files`]) and while
    /// [`Engine::discard_own`] checks memory and discards; see there.
    files: Mutex<()>,
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
            state: Mutex::default(),
            files: Mutex::default(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The files lock; taken before the state lock, never inside it.
    fn files(&self) -> MutexGuard<'_, ()> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A store read on the blocking pool. Writes go through
    /// [`Engine::in_order`].
    pub(crate) async fn with_store<T: Send + 'static>(
        &self,
        body: impl FnOnce(&Store) -> store::Result<T> + Send + 'static,
    ) -> store::Result<T> {
        on_blocking_pool(self.store.clone(), body).await
    }

    /// Runs `write` on the blocking pool once the write asked for before it
    /// has returned (`place`, from [`State::next_write`]). The pool runs
    /// its calls in any order, so without the line an older receipt could
    /// commit over a newer one, and a revoke's delete could remove the
    /// pairing asked for after it or commit before the pairing's save asked
    /// for before it, which leaves the revoked phone in the store. The
    /// write runs in a task of its own, so it keeps its place, and the
    /// writes behind it wait for it, also when the request that asked for
    /// it is dropped. A failed write does not hold up the next; reads do
    /// not wait. Swift: `HandoverEngine.inOrder`.
    async fn in_order<T: Send + 'static>(
        &self,
        place: InOrder,
        write: impl FnOnce(&Store) -> store::Result<T> + Send + 'static,
    ) -> store::Result<T> {
        let store = self.store.clone();
        tokio::spawn(async move {
            let InOrder { previous, done } = place;
            if let Some(previous) = previous {
                // Closed, not sent: the write before has returned.
                let _ = previous.await;
            }
            let written = on_blocking_pool(store, write).await;
            drop(done);
            written
        })
        .await
        .map_err(|error| join_error(&error))?
    }

    /// On start, drop inbox files no receipt accounts for (a crash between
    /// announce and the first save, a device revoked while offline), that a
    /// completed intake left behind (it copied the file before writing
    /// `complete`), or whose receipt has not moved in
    /// [`Engine::ABANDONED_AFTER_SECONDS`]. Best effort: a receipt the
    /// store cannot read keeps its files, which a later start sweeps.
    pub async fn sweep_orphans(&self) {
        let _ = self.inbox.prepare();
        let cutoff = (self.now)() - Duration::seconds(Self::ABANDONED_AFTER_SECONDS);
        for recording_id in self.inbox.recording_ids() {
            let Ok(receipt) = self
                .with_store(move |store| store.handover_receipt(recording_id))
                .await
            else {
                continue;
            };
            match receipt {
                None => self.inbox.discard(recording_id),
                Some(receipt)
                    if receipt.state.kind() == HandoverStateKind::Complete
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
    /// any open window.
    pub fn begin_pairing(&self) -> PairingPayload {
        let session = PairingSession::open(
            self.identity.mac_id(),
            &self.configuration.service_name,
            &self.identity.fingerprint(),
            self.configuration.pairing_window,
            self.now.clone(),
        );
        let payload = session.payload.clone();
        let mut state = self.state();
        state.window += 1;
        state.pairing = Some(session);
        payload
    }

    pub fn cancel_pairing(&self) {
        let mut state = self.state();
        state.pairing = None;
        state.window += 1;
    }

    #[must_use]
    pub fn pairing_is_open(&self) -> bool {
        self.state()
            .pairing
            .as_ref()
            .is_some_and(PairingSession::is_open)
    }

    /// Forgets the device and drops what it was uploading: the files of its
    /// receipts in memory, unless another phone announced the same
    /// recording id once they left memory (`Engine::discard_own`). A
    /// `complete` in flight that has not reached the intake answers 401
    /// when it sees the revoke, and discards the files when its receipt was
    /// only in the store, where the revoke does not look. An admission past
    /// that check may still finish; the revoke's discard can also make it
    /// fail. Its `complete` receipt stays out of memory while the device is
    /// revoked and out of the store while the device is gone from it; once
    /// the phone paired again it is written like any other. Files of a
    /// receipt only in the store (not read since start) wait for the next
    /// start's sweep.
    ///
    /// A failed store delete leaves the device paired in the store but
    /// revoked in memory: its recording routes answer 401 until it pairs
    /// again, a retried revoke finishes or the app restarts, and the
    /// receipts are published without its own. A half-revoked phone that
    /// cannot hand over is safer than one that can.
    pub async fn revoke(&self, device_id: Uuid) -> store::Result<()> {
        let mut unfinished = Vec::new();
        let place = {
            // Before the first yield: a `complete` that starts or checks
            // while the store delete runs must already see this revoke. The
            // delete's place in line goes with the count, so a pairing that
            // read the count before the bump saves before the delete, and
            // one that read it after saves after it.
            let mut state = self.state();
            state.revoked.insert(device_id);
            *state.revocations.entry(device_id).or_default() += 1;
            state.active_receipts.retain(|_, receipt| {
                let owned = receipt.device_id == device_id;
                if owned && receipt.state.kind() != HandoverStateKind::Complete {
                    unfinished.push(receipt.recording_id);
                }
                !owned
            });
            state.next_write()
        };
        for recording_id in unfinished {
            self.discard_own(recording_id, device_id, false);
        }
        let deleted = self
            .in_order(place, move |store| store.delete_paired_device(device_id))
            .await;
        self.publish_receipts();
        deleted
    }

    // Auth gate

    /// Refreshes `last_seen_at`, at most once a minute, in line with the
    /// other store writes. The gate read `device` before a yield, so the
    /// write is an `UPDATE` of the row that still holds `token_hash`: a
    /// revoke that landed in between is not undone, and the device is not
    /// re-inserted. Public for the tests, which run it against a device
    /// revoked after its read.
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
        let place = self.state().next_write();
        let _ = self
            .in_order(place, move |store| {
                store.touch_paired_device(id, &token_hash, timestamp)
            })
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
        let Principal::Pairing(window) = request.principal else {
            return Self::pairing_rejected();
        };
        // The gate passed at the head; the window may have closed, or been
        // replaced, since. Single use: one guard from the check to the
        // take, so two connections on two worker threads that both passed
        // the gate cannot both find the session open. Nothing in between
        // yields; a second request with the same secret that arrives while
        // the save runs finds no session and is 403. A failed save reopens
        // the window, unless it was cancelled or replaced while the save ran.
        let (session, device_id, name) = {
            let mut state = self.state();
            match &state.pairing {
                Some(session) if session.is_open() && state.window == window => {}
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
        // The count and the save's place in line under one guard: a revoke
        // counted here deletes before the save, one that starts later
        // deletes after it and keeps the device revoked.
        let (revocation, place) = {
            let mut state = self.state();
            (state.revocation_count(device_id), state.next_write())
        };
        if let Err(error) = self
            .in_order(place, move |store| store.save_paired_device(&device, &hash))
            .await
        {
            let mut state = self.state();
            if state.pairing.is_none() && state.window == window {
                state.pairing = Some(session);
            }
            return HandoverResponse::internal_error("saving the device", &error);
        }
        // A revoke that started during the save deletes the device after
        // it and keeps it revoked; the next pairing clears it.
        {
            let mut state = self.state();
            if state.revocation_count(device_id) == revocation {
                state.revoked.remove(&device_id);
            }
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

    /// Drops the receipt of `recording_id` from memory on behalf of
    /// `device_id` and tells the observers, unless memory holds another
    /// device's receipt of it ([`State::holds_another_devices`]); the store
    /// row, if any, stays. Swift: `HandoverEngine.forget`, which goes by the
    /// recording id alone.
    pub(crate) fn forget_own(&self, recording_id: Uuid, device_id: Uuid) {
        {
            let mut state = self.state();
            if state.holds_another_devices(recording_id, device_id) {
                return;
            }
            state.active_receipts.remove(&recording_id);
        }
        self.publish_receipts();
    }

    /// Discards every inbox file of `recording_id` on behalf of
    /// `device_id`, and with `forget` drops its receipt from memory as
    /// [`Engine::forget_own`] does, unless memory holds another device's
    /// receipt of it: those files are that phone's upload. Every discard of
    /// the engine goes through here, except the sweep before the listener
    /// starts. The check and the discard run under the files lock. A
    /// recording's files are created
    /// only by a request whose device's receipt memory holds by then (the
    /// announce whose change made the receipt, or a re-announce of its
    /// owner), and only under the same lock, so another phone's announce
    /// either made its receipt before the check, and its files stay, or
    /// opens them once the discard is over. Swift: the actor makes the
    /// check and the discard one step.
    pub(crate) fn discard_own(&self, recording_id: Uuid, device_id: Uuid, forget: bool) {
        let files = self.files();
        {
            let mut state = self.state();
            if state.holds_another_devices(recording_id, device_id) {
                return;
            }
            if forget {
                state.active_receipts.remove(&recording_id);
            }
        }
        self.inbox.discard(recording_id);
        drop(files);
        if forget {
            self.publish_receipts();
        }
    }

    /// Opens the files of a recording whose receipt this request just made:
    /// an empty partial (or the one there, never truncated) and the
    /// metadata sidecar, under the files lock ([`Engine::discard_own`]).
    pub(crate) fn open_files(&self, metadata: &RecordingMetadata) -> std::io::Result<()> {
        let _files = self.files();
        self.inbox.begin(metadata)
    }

    /// Opens the files of a known recording again when they are gone (a
    /// sweep, a crash before the first chunk, a refusal) and no verified
    /// file waits for a second intake attempt; true when it opened them.
    /// The check and the creation are one step under the files lock, so two
    /// re-announces, or a re-announce and the announce that made the
    /// receipt, never write the sidecar over a partial another one opened.
    pub(crate) fn reopen_missing_files(
        &self,
        metadata: &RecordingMetadata,
    ) -> std::io::Result<bool> {
        let _files = self.files();
        let recording_id = metadata.recording_id;
        if self.inbox.has_verified(recording_id, metadata.format)
            || (self.inbox.has_partial(recording_id)
                && self.inbox.load_metadata(recording_id).is_some())
        {
            return Ok(false);
        }
        self.inbox.begin(metadata)?;
        Ok(true)
    }

    /// One state change through [`Engine::update`]: the state and, when
    /// given, the chunk set, then the save. Callers that answer the phone
    /// whatever the write did ignore the result deliberately: memory
    /// already holds the change, or the newer receipt that declined it, and
    /// the phone's next request re-reads.
    /// Swift: `HandoverEngine.transition`, where the actor makes the read
    /// and the write one step.
    pub(crate) async fn transition(
        &self,
        receipt: &mut HandoverReceipt,
        state: HandoverState,
        received_chunks: Option<Vec<i64>>,
    ) -> store::Result<()> {
        self.update(receipt, |edit| {
            edit.state = state;
            if let Some(received_chunks) = received_chunks {
                edit.received_chunks = received_chunks;
            }
        })
        .await
    }

    /// A change of the receipt a request read, through [`Engine::change`]:
    /// `edit` changes the copy memory holds, or `receipt` when memory holds
    /// none (a revoked device), and `receipt` comes back as changed. When
    /// memory holds another device's receipt (another phone announced the
    /// same recording id), nothing changes, `receipt` included. A receipt memory holds
    /// as `complete` stays as it is, nothing is saved and `receipt` comes
    /// back as memory holds it: a request that read it before the phone's
    /// `complete` admitted the recording must not put it back, or the
    /// phone's next `complete` would start over and admit it again.
    async fn update(
        &self,
        receipt: &mut HandoverReceipt,
        edit: impl FnOnce(&mut HandoverReceipt),
    ) -> store::Result<()> {
        let picked = self.change(
            receipt.recording_id,
            |held| match held {
                Some(held) if held.device_id != receipt.device_id => Err(None),
                Some(held) if held.state.kind() == HandoverStateKind::Complete => {
                    Err(Some(Box::new(held.clone())))
                }
                held => Ok(held.unwrap_or(receipt).clone()),
            },
            edit,
        );
        match picked {
            Ok((changed, place)) => {
                *receipt = changed.clone();
                self.save(changed, place).await
            }
            Err(complete) => {
                if let Some(complete) = complete {
                    *receipt = *complete;
                }
                Ok(())
            }
        }
    }

    /// Folds chunk `index` into the receipt as memory holds it, sets it to
    /// `receiving` and saves it. `None`, with nothing changed, when memory
    /// holds no receipt of `device_id` for `recording_id` (revoked,
    /// forgotten). A receipt memory holds as `complete` stays as it is, as
    /// in [`Engine::update`], and the chunk counts as received:
    /// `Some(Ok(()))` with nothing saved.
    pub(crate) async fn add_chunk(
        &self,
        recording_id: Uuid,
        device_id: Uuid,
        index: i64,
    ) -> Option<store::Result<()>> {
        let picked = self.change(
            recording_id,
            |held| match held.filter(|held| held.device_id == device_id) {
                None => Err(None),
                Some(held) if held.state.kind() == HandoverStateKind::Complete => Err(Some(Ok(()))),
                Some(held) => Ok(held.clone()),
            },
            |edit| {
                edit.state = HandoverState::Receiving;
                edit.received_chunks.push(index);
                edit.received_chunks.sort_unstable();
                edit.received_chunks.dedup();
            },
        );
        match picked {
            Ok((changed, place)) => Some(self.save(changed, place).await),
            Err(declined) => declined,
        }
    }

    /// Runs `edit` on the copy `pick` makes of the receipt memory holds for
    /// `recording_id` (`pick` gets `None` when memory holds none), moves
    /// `updated_at`, keeps the result in memory and takes the save's place
    /// in line, all under one guard. When `pick` declines, nothing changes,
    /// no place is taken and its `Err` comes back. If the read and the
    /// write were two steps, two requests on two threads could start from
    /// the same copy, and the later one would drop the other's change in
    /// memory and in the store. The clock is read before the guard, so a
    /// slow clock never holds the lock.
    fn change<Declined>(
        &self,
        recording_id: Uuid,
        pick: impl FnOnce(Option<&HandoverReceipt>) -> Result<HandoverReceipt, Declined>,
        edit: impl FnOnce(&mut HandoverReceipt),
    ) -> Result<(HandoverReceipt, InOrder), Declined> {
        let timestamp = (self.now)();
        let mut state = self.state();
        let mut changed = pick(state.active_receipts.get(&recording_id))?;
        edit(&mut changed);
        changed.updated_at = timestamp;
        state.remember(&changed);
        Ok((changed, state.next_write()))
    }

    /// Saves `receipt` at its place in line and tells the observers.
    async fn save(&self, receipt: HandoverReceipt, place: InOrder) -> store::Result<()> {
        let result = self
            .in_order(place, move |store| store.save_handover_receipt(&receipt))
            .await;
        self.publish_receipts();
        result
    }

    /// The receipt from memory or the store. Another request may have loaded
    /// and advanced it while the store read ran; memory wins then. A failed
    /// read is an error, not "no receipt": taken for a new recording, it
    /// would let an announce overwrite a `complete` receipt and the next
    /// `complete` admit the meeting a second time.
    pub(crate) async fn receipt(
        &self,
        recording_id: Uuid,
    ) -> store::Result<Option<HandoverReceipt>> {
        if let Some(active) = self.state().active_receipts.get(&recording_id) {
            return Ok(Some(active.clone()));
        }
        let Some(stored) = self
            .with_store(move |store| store.handover_receipt(recording_id))
            .await?
        else {
            return Ok(None);
        };
        let mut state = self.state();
        if let Some(active) = state.active_receipts.get(&recording_id) {
            return Ok(Some(active.clone()));
        }
        state.remember(&stored);
        Ok(Some(stored))
    }

    /// The receipt when it belongs to the requesting device; the answer
    /// for the phone when there is none (404) or the read failed (500).
    pub(crate) async fn owned_receipt(
        &self,
        recording_id: Uuid,
        device: &PairedDevice,
    ) -> Result<HandoverReceipt, HandoverResponse> {
        match self.receipt(recording_id).await {
            Ok(Some(receipt)) if receipt.device_id == device.id => Ok(receipt),
            Ok(_) => Err(recording::no_such_recording()),
            Err(error) => Err(HandoverResponse::internal_error(
                "reading the receipt",
                &error,
            )),
        }
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

/// `body` on the blocking pool; a panic there is an I/O error.
async fn on_blocking_pool<T: Send + 'static>(
    store: Arc<Store>,
    body: impl FnOnce(&Store) -> store::Result<T> + Send + 'static,
) -> store::Result<T> {
    tokio::task::spawn_blocking(move || body(&store))
        .await
        .map_err(|error| join_error(&error))?
}

/// A task that panicked or was cancelled, as a store I/O error.
fn join_error(error: &tokio::task::JoinError) -> store::StoreError {
    store::StoreError::Io(std::io::Error::other(error.to_string()))
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
                let window = Self::credential("Pairing", authorization).and_then(|secret| {
                    let state = self.state();
                    let session = state.pairing.as_ref()?;
                    session.matches(secret).then_some(state.window)
                });
                match window {
                    Some(window) => AuthOutcome::Allowed(Principal::Pairing(window)),
                    None => AuthOutcome::Rejected(Self::pairing_rejected()),
                }
            }
            AuthRequirement::Bearer => {
                let Some(token) = Self::credential("Bearer", authorization) else {
                    return AuthOutcome::Rejected(Self::unauthorized());
                };
                let hash = DeviceTokens::hash(token);
                let lookup = hash.clone();
                // A failed read is a 500, not a 401: the phone takes 401
                // for a revoke and unpairs.
                match self
                    .with_store(move |store| store.paired_device_for_token_hash(&lookup))
                    .await
                {
                    Ok(Some(device)) => {
                        AuthOutcome::Allowed(Principal::Device(self.touch(device, hash).await))
                    }
                    Ok(None) => AuthOutcome::Rejected(Self::unauthorized()),
                    Err(error) => AuthOutcome::Rejected(HandoverResponse::internal_error(
                        "reading the device",
                        &error,
                    )),
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
            // A device revoked in memory may still pass the gate: its store
            // delete failed or has not committed yet. Its recording routes
            // stop here; unpair stays open so the phone can still drop its
            // pairing.
            (_, Some(device)) if self.state().revoked.contains(&device.id) => Self::unauthorized(),
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
