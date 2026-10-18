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
            |held| {
                #[cfg(test)]
                assert!(
                    self.files.try_lock().is_err(),
                    "an announce makes its receipt under the files lock"
                );
                match held {
                    Some(held) if Some(held) != expected => Err(Box::new(held.clone())),
                    _ => Ok(fresh),
                }
            },
            |_| {},
        )?;
        #[cfg(test)]
        tests::meanwhile(self, receipt.recording_id);
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

#[cfg(test)]
mod tests {
    //! White-box checks of what a request on another thread can leave in the
    //! synchronous stretches the integration tests cannot gate.

    use std::cell::RefCell;
    use std::sync::Arc;

    use chrono::{DateTime, TimeZone as _, Utc};
    use steno_core::testing::FakeHandoverIntake;
    use steno_core::{
        AudioFormat, HandoverReceipt, HandoverState, PairedDevice, RecordingMetadata, Store,
    };
    use tokio::sync::watch;
    use uuid::Uuid;

    use crate::configuration::HandoverConfiguration;
    use crate::engine::Engine;
    use crate::identity::HandoverIdentity;

    fn device(name: &str) -> PairedDevice {
        PairedDevice {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            paired_at: Utc.timestamp_opt(1_789_990_000, 0).unwrap(),
            last_seen_at: None,
        }
    }

    fn engine(directory: &std::path::Path, store: Arc<Store>, now: DateTime<Utc>) -> Arc<Engine> {
        let configuration = HandoverConfiguration {
            service_name: "Test".to_owned(),
            advertise: false,
            inbox_directory: directory.join("inbox"),
            ..HandoverConfiguration::default()
        };
        Engine::new(
            configuration,
            Arc::new(HandoverIdentity::mint("Test", now).unwrap()),
            store,
            Arc::new(FakeHandoverIntake::default()),
            watch::channel(Vec::new()).0,
            Arc::new(move || now),
        )
    }

    fn metadata(recording_id: Uuid, byte_count: i64, device_name: &str) -> RecordingMetadata {
        RecordingMetadata {
            recording_id,
            started_at: Utc.timestamp_opt(1_789_990_000, 0).unwrap(),
            duration_seconds: 1.0,
            byte_count,
            sha256: vec![7; 32],
            chunk_size: 64 * 1024,
            format: AudioFormat::M4aAac,
            device_name: device_name.to_owned(),
        }
    }

    /// A `completing` mark holds the engine only while it lives, so the weak
    /// self-handle leaves no cycle.
    #[tokio::test]
    async fn a_dropped_mark_leaves_the_engine_to_its_owner() {
        let directory = tempfile::tempdir().unwrap();
        let engine = engine(
            directory.path(),
            Arc::new(Store::in_memory().unwrap()),
            Utc.timestamp_opt(1_790_000_000, 0).unwrap(),
        );
        let mark = engine
            .begin_completing(Uuid::new_v4())
            .expect("not completing yet");
        assert_eq!(Arc::strong_count(&engine), 2, "the mark holds the engine");
        drop(mark);
        assert_eq!(Arc::strong_count(&engine), 1, "only the owner's");
    }

    #[tokio::test]
    async fn a_refusal_leaves_another_devices_receipt_and_files_alone() {
        // X's revoke landed during X's `complete` read; Y then announced the same
        // id. The refusal leaves Y's upload alone.
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let x = device("X");
        let y = device("Y");
        store.save_paired_device(&x, &[1; 32]).unwrap();
        store.save_paired_device(&y, &[2; 32]).unwrap();
        let engine = engine(
            directory.path(),
            store,
            Utc.timestamp_opt(1_790_000_000, 0).unwrap(),
        );

        // The count X's `complete` takes before its receipt read.
        let revocation = engine.state().revocation_count(x.id);
        engine.revoke(x.id).await.unwrap();
        let recording_id = Uuid::new_v4();
        let metadata = metadata(recording_id, 3, "Y");
        let announced = engine
            .announce(recording_id, &y, &serde_json::to_vec(&metadata).unwrap())
            .await;
        assert_eq!(announced.status, http::StatusCode::CREATED);

        let refused = engine.refusal(recording_id, &x, revocation);
        assert_eq!(
            refused.map(|response| response.status),
            Some(http::StatusCode::UNAUTHORIZED)
        );
        let held = engine.state().active_receipts.get(&recording_id).cloned();
        assert_eq!(
            held.map(|receipt| receipt.device_id),
            Some(y.id),
            "Y's receipt stays"
        );
        assert!(engine.inbox.has_partial(recording_id), "Y's partial stays");
        assert_eq!(
            engine.inbox.load_metadata(recording_id),
            Some(metadata),
            "and so does its sidecar"
        );
    }

    #[tokio::test]
    async fn a_chunk_of_a_replaced_upload_leaves_the_new_receipt_alone() {
        // A chunk folded in after the phone re-split or replaced the upload leaves
        // the new receipt alone.
        for resplit in [true, false] {
            let directory = tempfile::tempdir().unwrap();
            let store = Arc::new(Store::in_memory().unwrap());
            let y = device("Y");
            store.save_paired_device(&y, &[2; 32]).unwrap();
            let engine = engine(
                directory.path(),
                store,
                Utc.timestamp_opt(1_790_000_000, 0).unwrap(),
            );
            let recording_id = Uuid::new_v4();
            let first = metadata(recording_id, 300_000, "Y");
            let announced = engine
                .announce(recording_id, &y, &serde_json::to_vec(&first).unwrap())
                .await;
            assert_eq!(announced.status, http::StatusCode::CREATED);
            let read = engine.state().active_receipts[&recording_id].clone();

            let next = if resplit {
                RecordingMetadata {
                    chunk_size: 2 * first.chunk_size,
                    ..first.clone()
                }
            } else {
                RecordingMetadata {
                    sha256: vec![8; 32],
                    ..first.clone()
                }
            };
            let replaced = engine
                .announce(recording_id, &y, &serde_json::to_vec(&next).unwrap())
                .await;
            assert!(replaced.status.is_success(), "{replaced:?}");

            assert!(
                engine.add_chunk(&read, 0).await.is_none(),
                "the chunk of the earlier upload is refused (resplit: {resplit})"
            );
            let held = engine.state().active_receipts[&recording_id].clone();
            assert_eq!(
                (held.chunk_size, held.sha256, held.received_chunks),
                (next.chunk_size, next.sha256, vec![]),
                "resplit: {resplit}"
            );
        }
    }

    #[tokio::test]
    async fn a_late_complete_of_another_split_leaves_the_new_chunk_set_alone() {
        // A late `complete` of the first split, finding its partial replaced, must
        // not empty the new split's chunk set.
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let y = device("Y");
        store.save_paired_device(&y, &[2; 32]).unwrap();
        let engine = engine(
            directory.path(),
            store,
            Utc.timestamp_opt(1_790_000_000, 0).unwrap(),
        );
        let recording_id = Uuid::new_v4();
        let first = metadata(recording_id, 300_000, "Y");
        let announced = engine
            .announce(recording_id, &y, &serde_json::to_vec(&first).unwrap())
            .await;
        assert_eq!(announced.status, http::StatusCode::CREATED);
        let mut stale = engine.state().active_receipts[&recording_id].clone();

        let resplit = RecordingMetadata {
            chunk_size: 2 * first.chunk_size,
            ..first.clone()
        };
        let restarted = engine
            .announce(recording_id, &y, &serde_json::to_vec(&resplit).unwrap())
            .await;
        assert_eq!(restarted.status, http::StatusCode::OK);
        let read = engine.state().active_receipts[&recording_id].clone();
        assert!(matches!(engine.add_chunk(&read, 0).await, Some(Ok(()))));

        let answered = engine.replaced_during_the_verify(&mut stale).await;
        assert_eq!(answered.status, http::StatusCode::CONFLICT);
        let _ = engine
            .transition(
                &mut stale,
                HandoverState::Failed("sha256 mismatch".to_owned()),
                Some(Vec::new()),
            )
            .await;
        let _ = engine.update(&mut stale, None, Some(Vec::new())).await;
        let held = engine.state().active_receipts[&recording_id].clone();
        assert_eq!(
            (held.chunk_size, held.state, held.received_chunks),
            (resplit.chunk_size, HandoverState::Receiving, vec![0]),
            "the new split keeps its chunk"
        );
    }

    /// An edit of memory's receipt, as a request on another thread makes it.
    type Edit = Box<dyn FnOnce(&mut HandoverReceipt)>;

    thread_local! {
        /// The edit the next first announce runs between its `change` and opening.
        static MEANWHILE: RefCell<Option<Edit>> = const { RefCell::new(None) };
    }

    /// Runs the edit [`first_announce`] set for this thread, if any.
    pub(super) fn meanwhile(engine: &Engine, recording_id: Uuid) {
        if let Some(edit) = MEANWHILE.take() {
            edit(
                engine
                    .state()
                    .active_receipts
                    .get_mut(&recording_id)
                    .unwrap(),
            );
        }
    }

    /// Phone Y's announce of `ours`, which must find no receipt, with
    /// `meanwhile` run between its `change` and its opening.
    async fn first_announce(
        engine: &Engine,
        ours: &RecordingMetadata,
        y: &PairedDevice,
        meanwhile: impl FnOnce(&mut HandoverReceipt) + 'static,
    ) -> super::HandoverResponse {
        MEANWHILE.set(Some(Box::new(meanwhile)));
        let answered = engine
            .announce(ours.recording_id, y, &serde_json::to_vec(ours).unwrap())
            .await;
        assert!(
            MEANWHILE.take().is_none(),
            "the announce made its receipt and ran the edit"
        );
        answered
    }

    #[tokio::test]
    async fn a_first_announce_discards_the_partial_a_revoked_phone_left() {
        // X's partial is on disk with no receipt in memory; X is revoked and Y
        // announces the same id, discarding X's partial. X's stale `complete` is
        // then refused and leaves Y's files alone.
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let x = device("X");
        let y = device("Y");
        store.save_paired_device(&x, &[1; 32]).unwrap();
        store.save_paired_device(&y, &[2; 32]).unwrap();
        let engine = engine(
            directory.path(),
            store,
            Utc.timestamp_opt(1_790_000_000, 0).unwrap(),
        );
        let recording_id = Uuid::new_v4();
        engine
            .inbox
            .begin(&metadata(recording_id, 10, "X"))
            .unwrap();
        std::fs::write(engine.inbox.partial(recording_id), [9; 10]).unwrap();

        // The count X's `complete` takes before its receipt read.
        let revocation = engine.state().revocation_count(x.id);
        engine.revoke(x.id).await.unwrap();
        assert_eq!(
            std::fs::read(engine.inbox.partial(recording_id)).unwrap(),
            [9; 10],
            "the revoke found no receipt in memory and left X's partial"
        );
        let ours = metadata(recording_id, 3, "Y");
        let announced = engine
            .announce(recording_id, &y, &serde_json::to_vec(&ours).unwrap())
            .await;
        assert_eq!(announced.status, http::StatusCode::CREATED);
        assert_eq!(
            std::fs::read(engine.inbox.partial(recording_id)).unwrap(),
            Vec::<u8>::new(),
            "Y's partial starts empty"
        );
        assert_eq!(engine.inbox.load_metadata(recording_id), Some(ours.clone()));

        let refused = engine.refusal(recording_id, &x, revocation);
        assert_eq!(
            refused.map(|response| response.status),
            Some(http::StatusCode::UNAUTHORIZED)
        );
        assert!(engine.inbox.has_partial(recording_id), "Y's partial stays");
        assert_eq!(engine.inbox.load_metadata(recording_id), Some(ours));
    }

    #[tokio::test]
    async fn a_first_announce_whose_receipt_left_memory_before_its_opening_opens_nothing() {
        // Y's first announce made its receipt; before its opening, Y is revoked
        // and paired again and X's receipt is written back. Y's opening neither
        // discards X's files nor writes Y's sidecar over them.
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let x = device("X");
        let y = device("Y");
        store.save_paired_device(&x, &[1; 32]).unwrap();
        store.save_paired_device(&y, &[2; 32]).unwrap();
        let now = Utc.timestamp_opt(1_790_000_000, 0).unwrap();
        let engine = engine(directory.path(), store, now);
        let recording_id = Uuid::new_v4();
        let theirs = metadata(recording_id, 10, "X");
        engine.inbox.begin(&theirs).unwrap();
        std::fs::write(engine.inbox.partial(recording_id), [9; 4]).unwrap();

        let answered = first_announce(&engine, &metadata(recording_id, 3, "Y"), &y, move |held| {
            *held = HandoverReceipt {
                device_id: x.id,
                byte_count: theirs.byte_count,
                ..held.clone()
            };
        })
        .await;

        assert_eq!(
            answered.status,
            http::StatusCode::UNAUTHORIZED,
            "Y's announce is answered 401"
        );
        assert_eq!(
            std::fs::read(engine.inbox.partial(recording_id)).unwrap(),
            [9; 4],
            "X's partial stays"
        );
        assert_eq!(
            engine.inbox.load_metadata(recording_id),
            Some(theirs),
            "and so does X's sidecar"
        );
    }

    #[tokio::test]
    async fn a_first_announce_discards_a_waiting_verified_file_though_a_chunk_landed_first() {
        // A stale chunk lands in Y's fresh receipt before its opening; the opening
        // still discards the leftover verified file, so it is never admitted
        // unhashed.
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::in_memory().unwrap());
        let y = device("Y");
        store.save_paired_device(&y, &[2; 32]).unwrap();
        let now = Utc.timestamp_opt(1_790_000_000, 0).unwrap();
        let engine = engine(directory.path(), store, now);
        let recording_id = Uuid::new_v4();
        let ours = metadata(recording_id, 3, "Y");
        engine.inbox.prepare().unwrap();
        std::fs::write(engine.inbox.verified(recording_id, ours.format), [5; 8]).unwrap();

        let answered =
            first_announce(&engine, &ours, &y, |held| held.received_chunks.push(0)).await;
        assert_eq!(answered.status, http::StatusCode::CREATED);
        assert!(
            !engine.inbox.has_verified(recording_id, ours.format),
            "the waiting verified file is gone"
        );
        assert_eq!(
            std::fs::read(engine.inbox.partial(recording_id)).unwrap(),
            Vec::<u8>::new(),
            "Y's partial starts empty"
        );
        assert_eq!(engine.inbox.load_metadata(recording_id), Some(ours));
    }

    /// A store call that panics answers 500 like a failed one, and the
    /// connection lives on.
    #[tokio::test]
    async fn a_store_call_that_panics_is_an_io_error() {
        let directory = tempfile::tempdir().unwrap();
        let engine = engine(
            directory.path(),
            Arc::new(Store::in_memory().unwrap()),
            Utc::now(),
        );
        let read = engine
            .with_store(|_| -> steno_core::store::Result<()> { panic!("in the read") })
            .await;
        assert!(
            matches!(read, Err(steno_core::StoreError::Io(_))),
            "{read:?}"
        );
    }
}
