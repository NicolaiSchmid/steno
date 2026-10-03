//! The four recording routes: announce, status, chunk, complete. Every path
//! is idempotent on the recording id, so a phone that lost the answer can
//! simply repeat the call. `device` is the bearer principal the gate
//! established; a recording belongs to the device that announced it.
//! Swift: `Routing/RecordingHandler.swift`.

use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http::StatusCode;
use sha2::{Digest as _, Sha256};
use steno_core::{
    HandoverReceipt, HandoverState, HandoverStateKind, PairedDevice, RecordingMetadata,
};
use uuid::Uuid;

use super::{Engine, HandoverRequest, HandoverResponse};
use crate::pinning::constant_time_equals;
use crate::upload::{MetadataValidation, receiving_file};
use crate::wire;

enum Verification {
    File(PathBuf),
    Answered(HandoverResponse),
}

fn no_such_recording() -> HandoverResponse {
    HandoverResponse::problem(StatusCode::NOT_FOUND, "no such recording")
}

impl Engine {
    /// `PUT /v1/recordings/{id}` with `RecordingMetadata`: 201 for a new
    /// recording, 200 for a known one, both with `RecordingStatus`.
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

        if let Some(existing) = self.receipt(recording_id).await {
            if existing.device_id != device.id {
                return HandoverResponse::problem(
                    StatusCode::CONFLICT,
                    "another device owns this recording",
                );
            }
            return self.reannounce(existing, &metadata).await;
        }

        if let Err(error) = self.inbox.begin(&metadata) {
            return HandoverResponse::internal_error("opening the partial file", &error);
        }
        let timestamp = (self.now)();
        let receipt = HandoverReceipt {
            recording_id,
            device_id: device.id,
            state: HandoverState::Receiving,
            byte_count: metadata.byte_count,
            sha256: metadata.sha256.clone(),
            chunk_size: metadata.chunk_size,
            received_chunks: Vec::new(),
            created_at: timestamp,
            updated_at: timestamp,
        };
        if let Err(error) = self.persist(&receipt).await {
            self.inbox.discard(recording_id);
            return HandoverResponse::internal_error("saving the receipt", &error);
        }
        HandoverResponse::json(StatusCode::CREATED, &Self::status_of(&receipt))
    }

    /// A known recording announced again: 200 with the status, 409 when the
    /// metadata changed. The partial is reopened when it is gone (a sweep, a
    /// crash before the first chunk), with the same receipt and an empty
    /// chunk set; a verified file waiting for a second intake attempt keeps
    /// its chunk set, so the phone's retry (announce, then complete) sends no
    /// chunk twice.
    async fn reannounce(
        &self,
        mut receipt: HandoverReceipt,
        metadata: &RecordingMetadata,
    ) -> HandoverResponse {
        let recording_id = receipt.recording_id;
        if receipt.state.kind() == HandoverStateKind::Complete {
            return HandoverResponse::json(StatusCode::OK, &Self::status_of(&receipt));
        }
        if receipt.byte_count != metadata.byte_count
            || receipt.sha256 != metadata.sha256
            || receipt.chunk_size != metadata.chunk_size
        {
            return HandoverResponse::problem(
                StatusCode::CONFLICT,
                "metadata differs from the first announcement",
            );
        }
        let mut received_chunks = None;
        if !self.inbox.has_verified(recording_id, metadata.format)
            && (!self.inbox.has_partial(recording_id)
                || self.inbox.load_metadata(recording_id).is_none())
        {
            if let Err(error) = self.inbox.begin(metadata) {
                return HandoverResponse::internal_error("opening the partial file", &error);
            }
            received_chunks = Some(Vec::new());
        }
        if let Err(error) = self
            .transition(&mut receipt, HandoverState::Receiving, received_chunks)
            .await
        {
            return HandoverResponse::internal_error("saving the receipt", &error);
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
            Some(receipt) => HandoverResponse::json(StatusCode::OK, &Self::status_of(&receipt)),
            None => no_such_recording(),
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
        let Some(receipt) = self.owned_receipt(recording_id, device).await else {
            return no_such_recording();
        };
        if receipt.state.kind() == HandoverStateKind::Complete {
            return HandoverResponse::empty(StatusCode::NO_CONTENT);
        }
        // The router admits no negative index; the engine, driven directly,
        // checks both ends before `index * chunk_size` is computed.
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
            // Announce again: the partial is gone.
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
        // The write yielded: another chunk may have landed, or the device
        // may have been revoked. Fold this chunk into the receipt as it
        // stands now, never into the copy from before the write.
        let Some(mut receipt) = self
            .active_receipt(recording_id)
            .filter(|current| current.device_id == device.id)
        else {
            return no_such_recording();
        };
        let mut chunks = receipt.received_chunks.clone();
        chunks.push(index);
        chunks.sort_unstable();
        chunks.dedup();
        if let Err(error) = self
            .transition(&mut receipt, HandoverState::Receiving, Some(chunks))
            .await
        {
            return HandoverResponse::internal_error("saving the receipt", &error);
        }
        HandoverResponse::empty(StatusCode::NO_CONTENT)
    }

    /// `POST /v1/recordings/{id}/complete`: 200 `{meetingID}` once every
    /// chunk is present and the whole file hashes to the announced value;
    /// 409 with the status while chunks are missing or while an earlier
    /// `complete` is still verifying or admitting; 422 on a hash mismatch,
    /// after which the partial is gone and the phone starts over.
    pub(super) async fn complete(
        &self,
        recording_id: Uuid,
        device: &PairedDevice,
    ) -> HandoverResponse {
        let Some(mut receipt) = self.owned_receipt(recording_id, device).await else {
            return no_such_recording();
        };
        if let Some(meeting_id) = receipt.state.meeting_id() {
            return HandoverResponse::json(StatusCode::OK, &wire::CompleteResponse { meeting_id });
        }
        // One `complete` per recording at a time: the phone retries after
        // its own timeout, and a second verify or admission of the same file
        // must not start while the first is in flight. The phone answers a
        // 409 whose status lists every chunk by backing off.
        let Some(_completing) = self.begin_completing(recording_id) else {
            return HandoverResponse::json(StatusCode::CONFLICT, &Self::status_of(&receipt));
        };
        let Some(metadata) = self.inbox.load_metadata(recording_id) else {
            return HandoverResponse::problem(StatusCode::NOT_FOUND, "no metadata; announce again");
        };
        match self.verified_file(&mut receipt, &metadata).await {
            Verification::Answered(response) => response,
            Verification::File(file) => self.admit(&file, &metadata, device, &mut receipt).await,
        }
    }

    /// The verified file: the one already waiting after an earlier intake
    /// failure, else the partial once every chunk is present (409 with the
    /// status otherwise) and the whole file hashes to the announced value
    /// (422 and the partial is discarded otherwise), promoted to its final
    /// name.
    async fn verified_file(
        &self,
        receipt: &mut HandoverReceipt,
        metadata: &RecordingMetadata,
    ) -> Verification {
        let recording_id = receipt.recording_id;
        if self.inbox.has_verified(recording_id, metadata.format) {
            return Verification::File(self.inbox.verified(recording_id, metadata.format));
        }
        let count = MetadataValidation::chunk_count(receipt.byte_count, receipt.chunk_size);
        let every_chunk: Vec<i64> = (0..count).collect();
        let has_partial = self.inbox.has_partial(recording_id);
        if receipt.received_chunks != every_chunk || !has_partial {
            if !has_partial {
                let state = receipt.state.clone();
                let _ = self.transition(receipt, state, Some(Vec::new())).await;
            }
            return Verification::Answered(HandoverResponse::json(
                StatusCode::CONFLICT,
                &Self::status_of(receipt),
            ));
        }
        let _ = self
            .transition(receipt, HandoverState::Verifying, None)
            .await;

        let partial = self.inbox.partial(recording_id);
        let verified = match receiving_file::size(&partial) {
            Ok(size) if i64::try_from(size) == Ok(receipt.byte_count) => {
                receiving_file::hash_matches(partial, receipt.sha256.clone()).await
            }
            Ok(_) => Ok(false),
            Err(error) => Err(error),
        };
        let verified = match verified {
            Ok(verified) => verified,
            Err(error) => {
                return Verification::Answered(HandoverResponse::internal_error(
                    "verifying the file",
                    &error,
                ));
            }
        };
        self.refresh(receipt);
        if !verified {
            self.inbox.discard(recording_id);
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
        match self.inbox.promote(recording_id, metadata.format) {
            Ok(file) => Verification::File(file),
            Err(error) => Verification::Answered(HandoverResponse::internal_error(
                "moving the verified file",
                &error,
            )),
        }
    }

    /// Hands the verified file to the intake. On success the receipt is
    /// `complete` and the answer is 200 whatever the receipt write did: the
    /// real intake wrote this same receipt and deleted the file, a test
    /// intake did neither. The metadata sidecar is ours to remove; a
    /// replayed complete returns the same id through the early `complete`
    /// check. On failure the verified file stays for the phone's retry and
    /// the reason is fixed text, because the error may name the file's
    /// path.
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
                self.refresh(receipt);
                let _ = self
                    .transition(
                        receipt,
                        HandoverState::Failed(Self::INTAKE_REFUSED.to_owned()),
                        None,
                    )
                    .await;
                return HandoverResponse::internal_error("the intake", &error);
            }
        };
        self.refresh(receipt);
        let _ = self
            .transition(receipt, HandoverState::Complete { meeting_id }, None)
            .await;
        let _ = std::fs::remove_file(self.inbox.metadata(recording_id));
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
