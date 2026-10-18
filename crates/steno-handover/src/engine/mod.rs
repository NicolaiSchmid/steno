//! The protocol core behind the listener: the auth gate and every route,
//! independent of the connection and of TLS. One state lock, held for
//! synchronous sections only, never across a store, file or intake call; the
//! `files` lock serialises creating a recording's inbox files with discards
//! and is taken before the state lock, never inside it.

mod recording;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

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
    /// The secret matched pairing window number this; `/v1/pair` pairs only
    /// while that window is still the open one.
    Pairing(u64),
    Device(PairedDevice),
}

/// The outcome of the auth gate; a rejection carries the answer the
/// connection writes before it reads the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthOutcome {
    Allowed(Principal),
    Rejected(HandoverResponse),
}

/// One complete, authenticated request.
#[derive(Clone)]
pub struct HandoverRequest {
    pub route: Route,
    pub principal: Principal,
    pub headers: HeaderMap,
    pub body: Bytes,
}

/// Headers show as their names (`Authorization` carries secrets); the body
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

/// The body of a `/v1/pair` answer carries the bearer token, so it shows as
/// its length.
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

    /// A 500 whose body names the step only. The error carries the inbox path
    /// (the user's home directory), so it goes to the local log, never to the
    /// phone.
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

/// The engine as the connection sees it: a gate before the body, a handler
/// after it.
#[steno_core::async_trait]
pub trait RequestHandling: Send + Sync {
    async fn authenticate(&self, route: Route, authorization: Option<&str>) -> AuthOutcome;
    async fn handle(&self, request: HandoverRequest) -> HandoverResponse;
}

#[derive(Default)]
struct State {
    pairing: Option<PairingSession>,
    /// Bumped on every open and cancel, so a head authorised against an earlier
    /// window never pairs against a new one.
    window: u64,
    /// Receipts touched since start, by recording id.
    active_receipts: BTreeMap<Uuid, HandoverReceipt>,
    /// Recordings between the `verifying` write and the end of their
    /// admission, so a retried `complete` starts no second verify or admission.
    completing: BTreeSet<Uuid>,
    /// Devices revoked since start and not paired again: their receipts stay
    /// out of memory and their recording routes answer 401, also while the
    /// device is still in the store.
    revoked: BTreeSet<Uuid>,
    /// Revokes per device since start; a pairing never resets the count, so a
    /// `complete` from before the revoke still sees it.
    revocations: BTreeMap<Uuid, u64>,
    /// Closes once the store write asked for last has returned.
    last_write: Option<oneshot::Receiver<()>>,
}

/// A store write's place in line: `previous` closes once the write before
/// has returned; dropping `done` lets the next one go.
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

    /// Whether memory holds a receipt of `recording_id` from a device other
    /// than `device_id`; discards on behalf of `device_id` leave it alone.
    fn holds_another_devices(&self, recording_id: Uuid, device_id: Uuid) -> bool {
        self.active_receipts
            .get(&recording_id)
            .is_some_and(|receipt| receipt.device_id != device_id)
    }

    /// The receipts memory holds, oldest first.
    fn receipts_snapshot(&self) -> Vec<HandoverReceipt> {
        let mut receipts: Vec<HandoverReceipt> = self.active_receipts.values().cloned().collect();
        receipts.sort_by_key(|receipt| (receipt.created_at, receipt.recording_id));
        receipts
    }

    /// Drops the receipt on behalf of `device_id`; false, with nothing dropped,
    /// when memory holds another device's receipt of it.
    fn forget_own(&mut self, recording_id: Uuid, device_id: Uuid) -> bool {
        if self.holds_another_devices(recording_id, device_id) {
            return false;
        }
        self.active_receipts.remove(&recording_id);
        true
    }

    /// Keeps `receipt` as the live copy, unless its device was revoked.
    fn remember(&mut self, receipt: &HandoverReceipt) {
        if !self.revoked.contains(&receipt.device_id) {
            self.active_receipts
                .insert(receipt.recording_id, receipt.clone());
        }
    }

    /// The next place in the line of store writes, taken under the same guard
    /// as the memory change it stands for, so the store commits in the order
    /// memory changed. It must reach [`Engine::in_order`] before any yield or
    /// panic, or the write behind it goes at once.
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
    /// Held while a recording's inbox files are created and while the
    /// `discard_*` methods check memory and discard.
    files: Mutex<()>,
    /// A weak handle on the engine's own `Arc`, upgraded for the admission's
    /// task so it outlives a dropped request.
    me: Weak<Engine>,
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
    /// A partial whose receipt has not moved for this long is abandoned and
    /// swept; the receipt stays, and a late re-announce starts over.
    pub const ABANDONED_AFTER_SECONDS: i64 = 14 * 24 * 60 * 60;
    /// The `failed` reason after the intake refused; the detail is logged.
    pub const INTAKE_REFUSED: &'static str = "the intake refused the file";
    /// The `failed` reason of a stored `complete` receipt whose meeting is
    /// missing.
    pub const MEETING_MISSING: &'static str = "the admitted meeting is missing";

    /// The two answers of the gate. `unauthorized` tells the phone it was
    /// revoked; `pairing_rejected` means a bad, used or expired secret.
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
    ) -> Arc<Self> {
        Arc::new_cyclic(|me| Engine {
            inbox: Inbox::new(configuration.inbox_directory.clone()),
            configuration,
            identity,
            store,
            intake,
            now,
            receipts,
            state: Mutex::default(),
            files: Mutex::default(),
            me: me.clone(),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The files lock; taken before the state lock, never inside it.
    fn files(&self) -> MutexGuard<'_, ()> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A store read on the blocking pool; writes go through [`Engine::in_order`].
    pub(crate) async fn with_store<T: Send + 'static>(
        &self,
        body: impl FnOnce(&Store) -> store::Result<T> + Send + 'static,
    ) -> store::Result<T> {
        unless_it_panics(self.store.reading(body)).await
    }

    /// Runs `write` on the blocking pool once the write before it has returned.
    /// The pool runs calls in any order, so without the line an older receipt
    /// could commit over a newer one, or a revoke's delete race a pairing's
    /// save. The write runs in its own task, so it keeps its place when the
    /// request is dropped. A failed write does not hold up the next; reads do
    /// not wait.
    async fn in_order<T: Send + 'static>(
        &self,
        place: InOrder,
        write: impl FnOnce(&Store) -> store::Result<T> + Send + 'static,
    ) -> store::Result<T> {
        let store = self.store.clone();
        tokio::spawn(async move {
            let InOrder { previous, done } = place;
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let written = unless_it_panics(store.writing(write)).await;
            drop(done);
            written
        })
        .await
        .map_err(|error| join_error(&error))?
    }

    /// On start, drops inbox files no receipt accounts for, those a completed
    /// intake left behind, and those whose receipt has not moved in
    /// [`Engine::ABANDONED_AFTER_SECONDS`]. A `complete` receipt whose meeting
    /// is missing keeps its verified file for the phone's retry. Best effort:
    /// an unreadable receipt keeps its files.
    pub async fn sweep_orphans(&self) {
        let _ = self.inbox.prepare();
        let cutoff = (self.now)() - Duration::seconds(Self::ABANDONED_AFTER_SECONDS);
        for recording_id in self.inbox.recording_ids() {
            let Ok(receipt) = self
                .with_store(move |store| stored_receipt(store, recording_id))
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

    /// Opens a window, replacing any open one, and returns the QR payload.
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

    /// Forgets the device and drops the files of its receipts in memory
    /// (unless another phone announced the same recording id since). A
    /// `complete` in flight answers 401 when it sees the revoke; an admission
    /// past that check may still finish. Files of receipts only in the store
    /// wait for the next start's sweep.
    ///
    /// A failed store delete leaves the device paired in the store but revoked
    /// in memory: its recording routes answer 401 until it pairs again or the
    /// app restarts. A half-revoked phone that cannot hand over is safer than
    /// one that can.
    pub async fn revoke(&self, device_id: Uuid) -> store::Result<()> {
        let mut unfinished = Vec::new();
        let place = {
            // Before the first yield, so a `complete` running alongside the delete
            // sees this revoke. The delete's place in line goes with the count bump.
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
            self.discard_own(recording_id, device_id);
        }
        let deleted = self
            .in_order(place, move |store| store.delete_paired_device(device_id))
            .await;
        self.publish_receipts();
        deleted
    }

    // Auth gate

    /// Refreshes `last_seen_at`, at most once a minute, in line with the other
    /// writes. An `UPDATE` of the row that still holds `token_hash`, so a revoke
    /// in between is not undone. Public for the tests.
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

    /// The credential after `<scheme> ` in an `Authorization` header; the
    /// scheme is case insensitive.
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
        // The window may have closed or been replaced since the gate. One guard
        // from check to take, so two connections with the same secret cannot both
        // pair. A failed save reopens the window unless it changed meanwhile.
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
        // The count and the save's place in line under one guard, so a revoke is
        // ordered either before or after the save.
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
        // A revoke during the save deletes after it and keeps the device revoked.
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
        self.state().receipts_snapshot()
    }

    /// Sends the receipts to the observers; snapshot and send under one lock,
    /// so an older snapshot is never sent after a newer one.
    fn publish_receipts(&self) {
        let state = self.state();
        self.receipts.send_replace(state.receipts_snapshot());
    }

    /// Drops the receipt from memory on behalf of `device_id` and tells the
    /// observers, unless memory holds another device's receipt of it; the store
    /// row stays.
    pub(crate) fn forget_own(&self, recording_id: Uuid, device_id: Uuid) {
        let forgot = self.state().forget_own(recording_id, device_id);
        if forgot {
            self.publish_receipts();
        }
    }

    /// Discards every inbox file of `recording_id` on behalf of `device_id`,
    /// unless memory holds another device's receipt of it (those files are that
    /// phone's upload). Every discard except the start sweep and
    /// [`Engine::open_files`] goes through here or its two siblings.
    ///
    /// Check and discard are one step under the files lock, and files are only
    /// created under that lock while memory holds their receipt, so another
    /// phone's files either made their receipt before the check or open after
    /// the discard. A pairing again or a second revoke racing a re-announce can
    /// leave files with no receipt in memory; the next start's sweep or a first
    /// announce takes them.
    pub(crate) fn discard_own(&self, recording_id: Uuid, device_id: Uuid) {
        let _files = self.files();
        if !self.state().holds_another_devices(recording_id, device_id) {
            self.inbox.discard(recording_id);
        }
    }

    /// [`Engine::discard_own`] while `revoked` holds `device_id`, checked under
    /// the files lock so a new pairing's announce cannot open files in between.
    pub(crate) fn discard_own_while_revoked(&self, recording_id: Uuid, device_id: Uuid) {
        let _files = self.files();
        let discard = {
            let state = self.state();
            state.revoked.contains(&device_id)
                && !state.holds_another_devices(recording_id, device_id)
        };
        if discard {
            self.inbox.discard(recording_id);
        }
    }

    /// [`Engine::discard_own`] plus [`Engine::forget_own`] in one step under the
    /// files lock, so no other phone's announce makes its receipt in between.
    pub(crate) fn discard_and_forget_own(&self, recording_id: Uuid, device_id: Uuid) {
        {
            let _files = self.files();
            let forgot = self.state().forget_own(recording_id, device_id);
            if !forgot {
                return;
            }
            self.inbox.discard(recording_id);
        }
        self.publish_receipts();
    }

    /// The files lock for creating files on behalf of `device_id`; `None` while
    /// the device is revoked, as files made then would belong to no receipt in
    /// memory.
    fn files_for(&self, device_id: Uuid) -> Option<MutexGuard<'_, ()>> {
        let files = self.files();
        let revoked = self.state().revoked.contains(&device_id);
        (!revoked).then_some(files)
    }

    /// Opens the files of the receipt an announce just made for `device_id`:
    /// an empty partial and the metadata sidecar of `begin`, or none when
    /// `begin` is `None` (answered from the admission ledger). `_files` proves
    /// the caller holds the files lock, from before its `change` until this
    /// returns. `None`, with nothing opened, when memory no longer holds this
    /// device's receipt (revoked since).
    ///
    /// Every file of the recording id goes first: they belong to no live
    /// receipt, or to the replaced upload. Kept, `begin` would append to an old
    /// partial, or `complete` would hand an old verified file to the intake
    /// unhashed. No upload another device could still admit is lost: only a
    /// revoked or replaced request can still be at work on them, and the phone
    /// deletes its copy only on a 200 from `complete`.
    pub(crate) fn open_files(
        &self,
        _files: &MutexGuard<'_, ()>,
        recording_id: Uuid,
        device_id: Uuid,
        begin: Option<&RecordingMetadata>,
    ) -> Option<std::io::Result<()>> {
        let ours = self
            .state()
            .active_receipts
            .get(&recording_id)
            .is_some_and(|held| held.device_id == device_id);
        if !ours {
            return None;
        }
        self.inbox.discard(recording_id);
        Some(begin.map_or(Ok(()), |metadata| self.inbox.begin(metadata)))
    }

    /// Opens a known recording's files again when the partial or sidecar is
    /// gone and no verified file waits for a second intake; true when it opened
    /// them, `None` when the device was revoked. Check and creation are one
    /// step under the files lock.
    pub(crate) fn reopen_missing_files(
        &self,
        metadata: &RecordingMetadata,
        device_id: Uuid,
    ) -> Option<std::io::Result<bool>> {
        let _files = self.files_for(device_id)?;
        let recording_id = metadata.recording_id;
        if self.inbox.has_verified(recording_id, metadata.format)
            || (self.inbox.has_partial(recording_id)
                && self.inbox.load_metadata(recording_id).is_some())
        {
            return Some(Ok(false));
        }
        Some(self.inbox.begin(metadata).map(|()| true))
    }

    /// One state change through [`Engine::update`]: the state, optionally the
    /// chunk set, then the save. Callers that answer the phone regardless ignore
    /// the result: memory already holds the change, and the phone re-reads.
    pub(crate) async fn transition(
        &self,
        receipt: &mut HandoverReceipt,
        state: HandoverState,
        received_chunks: Option<Vec<i64>>,
    ) -> store::Result<()> {
        self.update(receipt, Some(state), received_chunks).await
    }

    /// Changes the receipt a request read: `state` and `received_chunks` apply
    /// to memory's copy, or to `receipt` when memory holds none (a revoked
    /// device). Nothing changes when memory holds another upload
    /// ([`same_upload`]), or holds the receipt in another chunk size (a chunk
    /// set is one split's). A receipt memory holds as `complete` stays as it
    /// is and comes back unsaved, so a stale request cannot make the phone
    /// admit it again.
    async fn update(
        &self,
        receipt: &mut HandoverReceipt,
        state: Option<HandoverState>,
        received_chunks: Option<Vec<i64>>,
    ) -> store::Result<()> {
        let writes_chunks = received_chunks.is_some();
        let picked = self.change(
            receipt.recording_id,
            |held| match held {
                Some(held) if !same_upload(held, receipt) => Err(None),
                Some(held) if held.state.kind() == HandoverStateKind::Complete => {
                    Err(Some(Box::new(held.clone())))
                }
                Some(held) if writes_chunks && held.chunk_size != receipt.chunk_size => Err(None),
                held => Ok(held.unwrap_or(receipt).clone()),
            },
            |edit| {
                if let Some(state) = state {
                    edit.state = state;
                }
                if let Some(received_chunks) = received_chunks {
                    edit.received_chunks = received_chunks;
                }
            },
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

    /// Folds chunk `index` into memory's receipt, sets it to `receiving` and
    /// saves it. `None` when memory holds no receipt of the same upload and
    /// split as `read`. A `complete` receipt stays as it is and the chunk counts
    /// as received.
    pub(crate) async fn add_chunk(
        &self,
        read: &HandoverReceipt,
        index: i64,
    ) -> Option<store::Result<()>> {
        let picked = self.change(
            read.recording_id,
            |held| match held
                .filter(|held| same_upload(held, read) && held.chunk_size == read.chunk_size)
            {
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

    /// Runs `edit` on the copy `pick` makes of memory's receipt, moves
    /// `updated_at`, keeps the result and takes the save's place in line, all
    /// under one guard, so two requests never start from the same copy. When
    /// `pick` declines, nothing changes and its `Err` comes back. The clock is
    /// read before the guard.
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

    /// The receipt from memory or the store (via [`stored_receipt`]); memory
    /// wins when both have it. A failed read is an error, not "no receipt",
    /// or an announce could overwrite a `complete` receipt.
    pub(crate) async fn receipt(
        &self,
        recording_id: Uuid,
    ) -> store::Result<Option<HandoverReceipt>> {
        if let Some(active) = self.state().active_receipts.get(&recording_id) {
            return Ok(Some(active.clone()));
        }
        let Some(stored) = self
            .with_store(move |store| stored_receipt(store, recording_id))
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

    /// The receipt when it belongs to the requesting device; otherwise the
    /// answer for the phone (404, or 500 on a failed read).
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
    pub(crate) fn begin_completing(&self, recording_id: Uuid) -> Option<Completing> {
        // Build the guard after the lock is released: its `Drop` takes the same
        // lock, and `then_some` would build it eagerly.
        let inserted = self.state().completing.insert(recording_id);
        inserted.then(|| Completing {
            engine: self
                .me
                .upgrade()
                .expect("a request reaches the engine through its Arc"),
            recording_id,
        })
    }
}

/// The stored receipt of `recording_id` as admitted or not. A `complete`
/// receipt whose meeting exists comes back as it is. Otherwise, when the
/// admission ledger holds its recording id, size and SHA-256, it reads as
/// `complete` with the ledger's meeting id, so a retry gets the first
/// meeting rather than a second admission. A `complete` receipt with
/// neither reads as `failed` ([`Engine::MEETING_MISSING`]): its phone never
/// got the 200, so the receipt must not answer 200 and the sweep must keep
/// the verified file.
fn stored_receipt(store: &Store, recording_id: Uuid) -> store::Result<Option<HandoverReceipt>> {
    let Some(mut receipt) = store.handover_receipt(recording_id)? else {
        return Ok(None);
    };
    if let Some(meeting_id) = receipt.state.meeting_id()
        && store.meeting(meeting_id)?.is_some()
    {
        return Ok(Some(receipt));
    }
    if let Some(meeting_id) =
        store.admitted_meeting(recording_id, receipt.byte_count, &receipt.sha256)?
    {
        receipt.state = HandoverState::Complete { meeting_id };
    } else if receipt.state.kind() == HandoverStateKind::Complete {
        receipt.state = HandoverState::Failed(Engine::MEETING_MISSING.to_owned());
    }
    Ok(Some(receipt))
}

/// Whether `held` is the upload `read` stands for: same device, same bytes.
/// The chunk size is not part of it; only chunk-set writes check that.
fn same_upload(held: &HandoverReceipt, read: &HandoverReceipt) -> bool {
    held.device_id == read.device_id
        && held.byte_count == read.byte_count
        && held.sha256 == read.sha256
}

/// A store call whose panic becomes an I/O error, so the request answers a
/// server error and the connection lives on.
async fn unless_it_panics<T>(
    call: impl std::future::Future<Output = store::Result<T>>,
) -> store::Result<T> {
    use futures_util::FutureExt as _;
    std::panic::AssertUnwindSafe(call)
        .catch_unwind()
        .await
        .unwrap_or_else(|_| {
            Err(store::StoreError::Io(std::io::Error::other(
                "the store call panicked",
            )))
        })
}

/// A task that panicked or was cancelled, as a store I/O error.
fn join_error(error: &tokio::task::JoinError) -> store::StoreError {
    store::StoreError::Io(std::io::Error::other(error.to_string()))
}

/// The `completing` mark, removed when `complete` returns or, once the
/// verified file went to the intake, when the admission's task ends.
pub(crate) struct Completing {
    engine: Arc<Engine>,
    recording_id: Uuid,
}

impl Drop for Completing {
    fn drop(&mut self) {
        self.engine.state().completing.remove(&self.recording_id);
    }
}

#[steno_core::async_trait]
impl RequestHandling for Engine {
    /// Runs at the request head, before the body.
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
            // A revoked device may still pass the gate (its store delete failed or is
            // pending); its recording routes stop here, unpair stays open.
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
