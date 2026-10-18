//! The four recording routes: announce, status, chunk, complete. Every path
//! is idempotent on the recording id, so a phone that lost the answer can
//! repeat the call. A recording belongs to the device that announced it.

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::StatusCode;
use sha2::{Digest as _, Sha256};
use steno_core::{
    HandoverReceipt, HandoverState, HandoverStateKind, PairedDevice, RecordingMetadata,
};
use uuid::Uuid;

use super::{Completing, Engine, HandoverRequest, HandoverResponse};
use crate::pinning::constant_time_equals;
use crate::upload::MetadataValidation;
use crate::upload::receiving_file;
use crate::wire;

enum Verification {
    File(PathBuf),
    Answered(HandoverResponse),
}

/// The receipt an announce made, the save's place in line, and what
/// [`Engine::open_files`] returned.
type MadeAndOpened = (HandoverReceipt, super::InOrder, Option<std::io::Result<()>>);

/// Where one decision of an announce ended.
enum Announced {
    Answered(HandoverResponse),
    /// Memory held another receipt by the time of the change; the announce
    /// decides again with that one.
    Changed(Box<HandoverReceipt>),
}

pub(super) fn no_such_recording() -> HandoverResponse {
    HandoverResponse::problem(StatusCode::NOT_FOUND, "no such recording")
}

impl Engine {
    /// `PUT /v1/recordings/{id}` with `RecordingMetadata`: 201 for a new
    /// recording (no receipt, or other bytes), 200 for a known one, both with
    /// `RecordingStatus`; 200 `complete` with every chunk listed for bytes the
    /// admission ledger shows admitted; 409 only for admitted bytes over another
    /// device's unfinished upload; 401 when the device was revoked since its
    /// receipt read.
    pub(super) async fn announce(
        &self,
        recording_id: Uuid,
        device: &PairedDevice,
        body: &[u8],
    ) -> HandoverResponse {
        let metadata: RecordingMetadata = match serde_json::from_slice(body) {
            Ok(metadata) => metadata,
            Err(error) => {
                return HandoverResponse::problem(
                    StatusCode::BAD_REQUEST,
                    format!("RecordingMetadata: {error}"),
                );
            }
        };
        if metadata.recording_id != recording_id {
            return HandoverResponse::problem(
                StatusCode::BAD_REQUEST,
                "recordingID does not match the path",
            );
        }
        if let Some(problem) = MetadataValidation::problem(&metadata, &self.configuration) {
            return HandoverResponse::problem(StatusCode::BAD_REQUEST, problem);
        }

        let mut existing = match self.receipt(recording_id).await {
            Ok(existing) => existing,
            Err(error) => return HandoverResponse::internal_error("reading the receipt", &error),
        };
        // Another request may have changed the receipt since the read; then this
        // announce decides again with the receipt memory holds.
        loop {
            let announced = match existing {
                None => self.new_bytes(None, device, &metadata).await,
                Some(existing) => self.reannounce(existing, device, &metadata).await,
            };
            match announced {
                Announced::Answered(response) => return response,
                Announced::Changed(held) => existing = Some(*held),
            }
        }
    }

    /// An announce of bytes the read receipt does not hold. The admission
    /// ledger decides: admitted bytes get a `complete` receipt with its meeting
    /// id (200, nothing opened), unless `read` is another device's unfinished
    /// upload, which that answer would make the other phone delete (409).
    /// Other bytes are a new recording under the id (201). Ledger rows are
    /// never deleted, so a row read stays true.
    async fn new_bytes(
        &self,
        read: Option<&HandoverReceipt>,
        device: &PairedDevice,
        metadata: &RecordingMetadata,
    ) -> Announced {
        let (recording_id, byte_count, sha256) = (
            metadata.recording_id,
            metadata.byte_count,
            metadata.sha256.clone(),
        );
        let admitted = match self
            .with_store(move |store| store.admitted_meeting(recording_id, byte_count, &sha256))
            .await
        {
            Ok(admitted) => admitted,
            Err(error) => {
                return Announced::Answered(HandoverResponse::internal_error(
                    "reading the admission ledger",
                    &error,
                ));
            }
        };
        let delivers = read.is_none_or(|read| {
            read.state.kind() == HandoverStateKind::Complete || read.device_id == device.id
        });
        match admitted {
            Some(meeting_id) if delivers => {
                let delivered =
                    self.fresh(device, metadata, HandoverState::Complete { meeting_id });
                self.replace(read, delivered, None, StatusCode::OK).await
            }
            Some(_) => Announced::Answered(HandoverResponse::problem(
                StatusCode::CONFLICT,
                "another device owns this recording",
            )),
            None => {
                let fresh = self.fresh(device, metadata, HandoverState::Receiving);
                self.replace(read, fresh, Some(metadata), StatusCode::CREATED)
                    .await
            }
        }
    }

    /// A receipt of `metadata`'s bytes for `device` in `state`, no chunk
    /// received.
    fn fresh(
        &self,
        device: &PairedDevice,
        metadata: &RecordingMetadata,
        state: HandoverState,
    ) -> HandoverReceipt {
        let timestamp = (self.now)();
        HandoverReceipt {
            recording_id: metadata.recording_id,
            device_id: device.id,
            state,
            byte_count: metadata.byte_count,
            sha256: metadata.sha256.clone(),
            chunk_size: metadata.chunk_size,
            received_chunks: Vec::new(),
            created_at: timestamp,
            updated_at: timestamp,
        }
    }

    /// Makes `fresh` the receipt in place of `expected` (the one the announce
    /// read), opens its files and answers with it; [`Announced::Changed`] when
    /// memory holds another receipt by then. The save happens whatever the
    /// opening did; a retried announce reopens missing files. A device revoked
    /// since its read opens none and is answered 401.
    async fn replace(
        &self,
        expected: Option<&HandoverReceipt>,
        fresh: HandoverReceipt,
        begin: Option<&RecordingMetadata>,
        status: StatusCode,
    ) -> Announced {
        let (recording_id, device_id) = (fresh.recording_id, fresh.device_id);
        let (receipt, place, opened) = match self.make_and_open(expected, fresh, begin) {
            Ok(made) => made,
            Err(held) => return Announced::Changed(held),
        };
        let saved = self.save(receipt.clone(), place).await;
        let Some(opened) = opened else {
            return Announced::Answered(Self::unauthorized());
        };
        if let Err(error) = saved {
            self.discard_own(recording_id, device_id);
            return Announced::Answered(HandoverResponse::internal_error(
                "saving the receipt",
                &error,
            ));
        }
        if let Err(error) = opened {
            return Announced::Answered(HandoverResponse::internal_error(
                "opening the partial file",
                &error,
            ));
        }
        Announced::Answered(HandoverResponse::json(status, &Self::status_of(&receipt)))
    }

    /// An announce's step under the files lock: [`Engine::change`] makes
    /// `fresh` the receipt, then [`Engine::open_files`] discards and opens the
    /// recording's files, so no other request touches them in between. Nothing
    /// in it yields; the lock is released before the save.
    fn make_and_open(
        &self,
        expected: Option<&HandoverReceipt>,
        fresh: HandoverReceipt,
        begin: Option<&RecordingMetadata>,
    ) -> Result<MadeAndOpened, Box<HandoverReceipt>> {
        let files = self.files();
        let (receipt, place) = self.change(
            fresh.recording_id,
            |held| match held {
                Some(held) if Some(held) != expected => Err(Box::new(held.clone())),
                _ => Ok(fresh),
            },
            |_| {},
        )?;
        let opened = self.open_files(&files, receipt.recording_id, receipt.device_id, begin);
        Ok((receipt, place, opened))
    }

    /// A known recording announced again. Other bytes go to
    /// [`Engine::new_bytes`], the same bytes from another device to
    /// [`Engine::take_over`]. For the owner, a `complete` receipt answers 200
    /// with every chunk; another split restarts the partial; else
    /// [`Engine::resume`].
    async fn reannounce(
        &self,
        receipt: HandoverReceipt,
        device: &PairedDevice,
        metadata: &RecordingMetadata,
    ) -> Announced {
        let complete = receipt.state.kind() == HandoverStateKind::Complete;
        if receipt.byte_count != metadata.byte_count || receipt.sha256 != metadata.sha256 {
            return self.new_bytes(Some(&receipt), device, metadata).await;
        }
        let receipt = if receipt.device_id == device.id {
            receipt
        } else {
            match self.take_over(&receipt, device).await {
                Ok(taken) => taken,
                Err(announced) => return announced,
            }
        };
        // Once `complete`, the split no longer matters. The answer lists every
        // chunk of the phone's split so it posts `complete`; the copy is never
        // saved.
        if complete {
            let resplit = HandoverReceipt {
                chunk_size: metadata.chunk_size,
                ..receipt
            };
            return Announced::Answered(HandoverResponse::json(
                StatusCode::OK,
                &Self::status_of(&resplit),
            ));
        }
        if receipt.chunk_size != metadata.chunk_size {
            let restarted = HandoverReceipt {
                state: HandoverState::Receiving,
                chunk_size: metadata.chunk_size,
                received_chunks: Vec::new(),
                ..receipt.clone()
            };
            return self
                .replace(Some(&receipt), restarted, Some(metadata), StatusCode::OK)
                .await;
        }
        Announced::Answered(self.resume(receipt, device, metadata).await)
    }

    /// The same bytes announced by another device take the receipt over, chunks
    /// and files kept: the same bytes are the same recording. The older
    /// device's requests then find no receipt of theirs (404).
    /// [`Announced::Changed`] when memory holds another receipt by then.
    async fn take_over(
        &self,
        receipt: &HandoverReceipt,
        device: &PairedDevice,
    ) -> Result<HandoverReceipt, Announced> {
        let (taken, place) = self
            .change(
                receipt.recording_id,
                |held| match held {
                    Some(held) if held != receipt => Err(Box::new(held.clone())),
                    _ => Ok(HandoverReceipt {
                        device_id: device.id,
                        ..receipt.clone()
                    }),
                },
                |_| {},
            )
            .map_err(Announced::Changed)?;
        if let Err(error) = self.save(taken.clone(), place).await {
            return Err(Announced::Answered(HandoverResponse::internal_error(
                "saving the receipt",
                &error,
            )));
        }
        Ok(taken)
    }

    /// The owner's re-announce of the same bytes and split: 200 with the
    /// status. A missing partial or sidecar is reopened with an empty chunk
    /// set; a verified file awaiting a second intake keeps it. 401 when the
    /// device was revoked since its read.
    async fn resume(
        &self,
        mut receipt: HandoverReceipt,
        device: &PairedDevice,
        metadata: &RecordingMetadata,
    ) -> HandoverResponse {
        let recording_id = receipt.recording_id;
        let received_chunks = match self.reopen_missing_files(metadata, device.id) {
            None => return Self::unauthorized(),
            Some(Ok(reopened)) => reopened.then(Vec::new),
            Some(Err(error)) => {
                return HandoverResponse::internal_error("opening the partial file", &error);
            }
        };
        if let Err(error) = self
            .transition(&mut receipt, HandoverState::Receiving, received_chunks)
            .await
        {
            return HandoverResponse::internal_error("saving the receipt", &error);
        }
        // Admitted meanwhile: drop the fresh files, or a stale `complete` would
        // verify an empty partial.
        if receipt.state.kind() == HandoverStateKind::Complete {
            self.discard_own(recording_id, device.id);
        }
        HandoverResponse::json(StatusCode::OK, &Self::status_of(&receipt))
    }

    /// `GET /v1/recordings/{id}`: the resume point, 404 for an unknown id.
    pub(super) async fn status(
        &self,
        recording_id: Uuid,
        device: &PairedDevice,
    ) -> HandoverResponse {
        match self.owned_receipt(recording_id, device).await {
            Ok(receipt) => HandoverResponse::json(StatusCode::OK, &Self::status_of(&receipt)),
            Err(response) => response,
        }
    }

    /// `PUT /v1/recordings/{id}/chunks/{n}` with raw bytes and
    /// `X-Steno-Chunk-SHA256`: 204, also for a chunk already received.
    pub(super) async fn receive_chunk(
        &self,
        recording_id: Uuid,
        index: i64,
        device: &PairedDevice,
        request: &HandoverRequest,
    ) -> HandoverResponse {
        let receipt = match self.owned_receipt(recording_id, device).await {
            Ok(receipt) => receipt,
            Err(response) => return response,
        };
        if receipt.state.kind() == HandoverStateKind::Complete {
            return HandoverResponse::empty(StatusCode::NO_CONTENT);
        }
        // The engine, driven directly, checks both ends before
        // `index * chunk_size` is computed.
        let count = MetadataValidation::chunk_count(receipt.byte_count, receipt.chunk_size);
        if index < 0 || index >= count {
            return HandoverResponse::problem(
                StatusCode::BAD_REQUEST,
                format!("chunk index must be below {count}"),
            );
        }
        let expected =
            MetadataValidation::chunk_length(index, receipt.byte_count, receipt.chunk_size);
        if i64::try_from(request.body.len()) != Ok(expected) {
            return HandoverResponse::problem(
                StatusCode::BAD_REQUEST,
                format!("chunk {index} must be {expected} bytes"),
            );
        }
        let digest = request
            .headers
            .get(wire::CHUNK_HASH_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| STANDARD.decode(value.trim()).ok())
            .filter(|digest| digest.len() == 32);
        let Some(digest) = digest else {
            return HandoverResponse::problem(
                StatusCode::BAD_REQUEST,
                format!(
                    "{} must be the base64 SHA-256 of the body",
                    wire::CHUNK_HASH_HEADER
                ),
            );
        };
        if !constant_time_equals(&Sha256::digest(&request.body), &digest) {
            return HandoverResponse::problem(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("chunk {index} hash mismatch"),
            );
        }
        if receipt.received_chunks.contains(&index) {
            return HandoverResponse::empty(StatusCode::NO_CONTENT);
        }
        if !self.inbox.has_partial(recording_id) {
            return HandoverResponse::problem(
                StatusCode::NOT_FOUND,
                "no partial file; announce again",
            );
        }
        #[allow(clippy::cast_sign_loss)]
        let offset = (index * receipt.chunk_size) as u64;
        if let Err(error) = receiving_file::write(
            request.body.clone(),
            offset,
            self.inbox.partial(recording_id),
        )
        .await
        {
            return HandoverResponse::internal_error("writing the chunk", &error);
        }
        // The write yielded, so fold this chunk into the receipt as it stands now.
        match self.add_chunk(&receipt, index).await {
            None => no_such_recording(),
            Some(Err(error)) => HandoverResponse::internal_error("saving the receipt", &error),
            Some(Ok(())) => HandoverResponse::empty(StatusCode::NO_CONTENT),
        }
    }

    /// `POST /v1/recordings/{id}/complete`: 200 `{meetingID}` once every chunk
    /// is present and the file hashes to the announced value; 409 with the
    /// status while chunks are missing, an earlier `complete` is in flight, or
    /// the partial was replaced during the verify; 422 on a hash mismatch (the
    /// partial is gone); 401 when the device was revoked.
    pub(super) async fn complete(
        &self,
        recording_id: Uuid,
        device: &PairedDevice,
    ) -> HandoverResponse {
        // Check the revoke again under the lock that takes the count: one may
        // have landed since `handle` checked.
        let revocation = {
            let state = self.state();
            if state.revoked.contains(&device.id) {
                return Self::unauthorized();
            }
            state.revocation_count(device.id)
        };
        let mut receipt = match self.owned_receipt(recording_id, device).await {
            Ok(receipt) => receipt,
            Err(response) => return response,
        };
        if let Some(meeting_id) = receipt.state.meeting_id() {
            // Admitted before the revoke, so answer it: a 401 would make the phone
            // keep the recording and upload a second meeting after pairing again.
            if self.revoked_since(device, revocation) {
                self.forget_own(recording_id, device.id);
            }
            return HandoverResponse::json(StatusCode::OK, &wire::CompleteResponse { meeting_id });
        }
        // Refuse before the `verifying` write, which would put a revoked row back.
        if let Some(refused) = self.refusal(recording_id, device, revocation) {
            return refused;
        }
        // One `complete` per recording at a time; the phone backs off on a 409
        // that lists every chunk.
        let Some(completing) = self.begin_completing(recording_id) else {
            return HandoverResponse::json(StatusCode::CONFLICT, &Self::status_of(&receipt));
        };
        let Some(metadata) = self.inbox.load_metadata(recording_id) else {
            return HandoverResponse::problem(StatusCode::NOT_FOUND, "no metadata; announce again");
        };
        match self
            .verified_file(&mut receipt, &metadata, device, revocation)
            .await
        {
            Verification::Answered(response) => response,
            Verification::File(file) => {
                completing
                    .admit(file, metadata, device.clone(), receipt)
                    .await
            }
        }
    }

    /// 401 when the device was revoked since `complete` took `revocation`.
    /// That revoke may have missed the receipt, so its files are discarded and
    /// it leaves memory, unless memory holds another device's receipt of it.
    fn refusal(
        &self,
        recording_id: Uuid,
        device: &PairedDevice,
        revocation: u64,
    ) -> Option<HandoverResponse> {
        if !self.revoked_since(device, revocation) {
            return None;
        }
        self.discard_and_forget_own(recording_id, device.id);
        Some(Self::unauthorized())
    }

    /// Whether the device's revoke count moved on from `revocation`.
    fn revoked_since(&self, device: &PairedDevice, revocation: u64) -> bool {
        self.state().revocation_count(device.id) != revocation
    }

    /// The verified file: one already waiting after an intake failure, else the
    /// partial once every chunk is present (409 otherwise) and its hash matches
    /// (422 and discarded otherwise), promoted to its final name. The partial
    /// stays open from before the `verifying` write to the promote so the hash
    /// is of the file the intake gets. 200 when another `complete` admitted it
    /// meanwhile; 401 when the device was revoked.
    async fn verified_file(
        &self,
        receipt: &mut HandoverReceipt,
        metadata: &RecordingMetadata,
        device: &PairedDevice,
        revocation: u64,
    ) -> Verification {
        let recording_id = receipt.recording_id;
        if self.inbox.has_verified(recording_id, metadata.format) {
            return Verification::File(self.inbox.verified(recording_id, metadata.format));
        }
        let count = MetadataValidation::chunk_count(receipt.byte_count, receipt.chunk_size);
        let every_chunk = receipt.received_chunks.iter().copied().eq(0..count);
        let has_partial = self.inbox.has_partial(recording_id);
        if !every_chunk || !has_partial {
            if !has_partial {
                // Keep the state memory holds; it may have changed since the read.
                let _ = self.update(receipt, None, Some(Vec::new())).await;
                if let Some(meeting_id) = receipt.state.meeting_id() {
                    return Verification::Answered(HandoverResponse::json(
                        StatusCode::OK,
                        &wire::CompleteResponse { meeting_id },
                    ));
                }
            }
            return Verification::Answered(HandoverResponse::json(
                StatusCode::CONFLICT,
                &Self::status_of(receipt),
            ));
        }
        // Opened before the first yield; a partial replaced meanwhile is another
        // file, which `Identity` tells apart.
        let partial = self.inbox.partial(recording_id);
        let opened = std::fs::File::open(&partial)
            .and_then(|file| Ok((receiving_file::Identity::of(&file)?, Arc::new(file))));
        let (identity, file) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                return Verification::Answered(HandoverResponse::internal_error(
                    "reading the partial",
                    &error,
                ));
            }
        };
        let _ = self
            .transition(receipt, HandoverState::Verifying, None)
            .await;
        // Another `complete` admitted it since our read.
        if let Some(meeting_id) = receipt.state.meeting_id() {
            return Verification::Answered(HandoverResponse::json(
                StatusCode::OK,
                &wire::CompleteResponse { meeting_id },
            ));
        }

        let verified = match file.metadata() {
            Ok(opened) if i64::try_from(opened.len()) == Ok(receipt.byte_count) => {
                receiving_file::hash_matches(file.clone(), receipt.sha256.clone()).await
            }
            Ok(_) => Ok(false),
            Err(error) => Err(error),
        };
        if self.revoked_since(device, revocation) {
            return Verification::Answered(self.revoked_during_the_verify(recording_id, device));
        }
        let verified = match verified {
            Ok(verified) => verified,
            Err(error) => {
                return Verification::Answered(HandoverResponse::internal_error(
                    "verifying the file",
                    &error,
                ));
            }
        };
        if receiving_file::Identity::at(&partial).ok() != Some(identity) {
            return Verification::Answered(self.replaced_during_the_verify(receipt).await);
        }
        if !verified {
            self.discard_own(recording_id, device.id);
            let _ = self
                .transition(
                    receipt,
                    HandoverState::Failed("sha256 mismatch".to_owned()),
                    Some(Vec::new()),
                )
                .await;
            return Verification::Answered(HandoverResponse::problem(
                StatusCode::UNPROCESSABLE_ENTITY,
                "sha256 mismatch; the partial was discarded",
            ));
        }
        let promoted = match self.inbox.promote(recording_id, metadata.format) {
            Ok(promoted) => promoted,
            Err(error) => {
                return Verification::Answered(HandoverResponse::internal_error(
                    "moving the verified file",
                    &error,
                ));
            }
        };
        // The file moved must still be the one hashed; one moved in its place goes.
        if receiving_file::Identity::at(&promoted).ok() != Some(identity) {
            let _ = std::fs::remove_file(&promoted);
            return Verification::Answered(self.replaced_during_the_verify(receipt).await);
        }
        // Closed before the intake, which moves or deletes the file.
        drop(file);
        Verification::File(promoted)
    }

    /// The 401 of a `complete` whose device was revoked during the verify,
    /// without a write, which could put back the deleted row. A partial there
    /// now goes while the device is still revoked; once paired again it is the
    /// new pairing's upload and stays.
    fn revoked_during_the_verify(
        &self,
        recording_id: Uuid,
        device: &PairedDevice,
    ) -> HandoverResponse {
        self.discard_own_while_revoked(recording_id, device.id);
        Self::unauthorized()
    }

    /// The 409 of a partial gone or recreated during the verify: the chunk set
    /// is emptied, so the phone sends every chunk again.
    async fn replaced_during_the_verify(&self, receipt: &mut HandoverReceipt) -> HandoverResponse {
        let _ = self
            .transition(receipt, HandoverState::Receiving, Some(Vec::new()))
            .await;
        HandoverResponse::json(StatusCode::CONFLICT, &Self::status_of(receipt))
    }

    /// Hands the verified file to the intake. On success the receipt is
    /// `complete`, the answer is 200 whatever the receipt write did, and every
    /// file of the recording goes (only the verified file when another device
    /// announced the id meanwhile). On failure the verified file stays for the
    /// retry, with a fixed reason because the error may name its path, unless
    /// memory now holds a receipt of other bytes, whose `complete` would hand
    /// this file over unhashed.
    async fn admit(
        &self,
        file: &std::path::Path,
        metadata: &RecordingMetadata,
        device: &PairedDevice,
        receipt: &mut HandoverReceipt,
    ) -> HandoverResponse {
        let recording_id = receipt.recording_id;
        let meeting_id = match self.intake.admit(file, metadata, device).await {
            Ok(meeting_id) => meeting_id,
            Err(error) => {
                let _ = self
                    .transition(
                        receipt,
                        HandoverState::Failed(Self::INTAKE_REFUSED.to_owned()),
                        None,
                    )
                    .await;
                let replaced =
                    self.state()
                        .active_receipts
                        .get(&recording_id)
                        .is_some_and(|held| {
                            held.byte_count != receipt.byte_count || held.sha256 != receipt.sha256
                        });
                if replaced {
                    let _ = std::fs::remove_file(file);
                }
                return HandoverResponse::internal_error("the intake", &error);
            }
        };
        let _ = self
            .transition(receipt, HandoverState::Complete { meeting_id }, None)
            .await;
        let _ = std::fs::remove_file(file);
        self.discard_own(recording_id, device.id);
        HandoverResponse::json(StatusCode::OK, &wire::CompleteResponse { meeting_id })
    }

    #[must_use]
    pub fn status_of(receipt: &HandoverReceipt) -> wire::RecordingStatus {
        if receipt.state.kind() == HandoverStateKind::Complete {
            let count = MetadataValidation::chunk_count(receipt.byte_count, receipt.chunk_size);
            return wire::RecordingStatus {
                state: HandoverStateKind::Complete,
                received_chunks: (0..count).collect(),
            };
        }
        wire::RecordingStatus {
            state: receipt.state.kind(),
            received_chunks: receipt.received_chunks.clone(),
        }
    }
}

impl Completing {
    /// [`Engine::admit`] in its own task, which holds this mark until the
    /// admission ends, so a phone hanging up drops the request, not the
    /// admission. Its retry gets a 409 while the mark is held and the final
    /// receipt after. A panic in the task resumes here.
    async fn admit(
        self,
        file: PathBuf,
        metadata: RecordingMetadata,
        device: PairedDevice,
        mut receipt: HandoverReceipt,
    ) -> HandoverResponse {
        let admission = tokio::spawn(async move {
            let response = self
                .engine
                .admit(&file, &metadata, &device, &mut receipt)
                .await;
            drop(self);
            response
        });
        match admission.await {
            Ok(response) => response,
            Err(join) => match join.try_into_panic() {
                Ok(panic) => std::panic::resume_unwind(panic),
                Err(cancelled) => HandoverResponse::internal_error("the intake", &cancelled),
            },
        }
    }
}
