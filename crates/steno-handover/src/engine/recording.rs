//! The four recording routes: announce, status, chunk, complete. Every path
//! is idempotent on the recording id, so a phone that lost the answer can
//! simply repeat the call. `device` is the bearer principal the gate
//! established; a recording belongs to the device that announced it.
//! Swift: `Routing/RecordingHandler.swift`.

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

use super::{Engine, HandoverRequest, HandoverResponse};
use crate::pinning::constant_time_equals;
use crate::upload::MetadataValidation;
use crate::upload::receiving_file;
use crate::wire;

enum Verification {
    File(PathBuf),
    Answered(HandoverResponse),
}

pub(super) fn no_such_recording() -> HandoverResponse {
    HandoverResponse::problem(StatusCode::NOT_FOUND, "no such recording")
}

impl Engine {
    /// `PUT /v1/recordings/{id}` with `RecordingMetadata`: 201 for a new
    /// recording, 200 for a known one, both with `RecordingStatus`; 401 when
    /// the device was revoked since its receipt read.
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

        let existing = match self.receipt(recording_id).await {
            Ok(existing) => existing,
            Err(error) => return HandoverResponse::internal_error("reading the receipt", &error),
        };
        if let Some(existing) = existing {
            return self.reannounce(existing, device, &metadata).await;
        }

        let timestamp = (self.now)();
        let fresh = HandoverReceipt {
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
        // A first announce of the same recording on another thread may have
        // made its receipt since the read above, and a chunk may have landed
        // in it. That receipt stays, and this announce is answered as if the
        // read had found it, without touching the files the other one opens.
        let picked = self.change(
            recording_id,
            |held| held.map_or(Ok(fresh), |held| Err(Box::new(held.clone()))),
            |_| {},
        );
        let (receipt, place) = match picked {
            Ok(picked) => picked,
            Err(held) => return self.reannounce(*held, device, &metadata).await,
        };
        // Only the announce that made the receipt opens the files, so the
        // sidecar is the metadata of the receipt memory holds. The place in
        // line goes to the save whatever `open_files` did, and nothing
        // between `change` and the save yields. A failed opening leaves the
        // receipt saved without files, so the phone's retried announce
        // reopens them as a re-announce. A device revoked since its receipt
        // read opens none and is answered 401: its receipt stayed out of
        // memory.
        let opened = self.open_files(&metadata, device.id);
        let saved = self.save(receipt.clone(), place).await;
        let Some(opened) = opened else {
            return Self::unauthorized();
        };
        if let Err(error) = saved {
            self.discard_own(recording_id, device.id);
            return HandoverResponse::internal_error("saving the receipt", &error);
        }
        if let Err(error) = opened {
            return HandoverResponse::internal_error("opening the partial file", &error);
        }
        HandoverResponse::json(StatusCode::CREATED, &Self::status_of(&receipt))
    }

    /// A known recording announced again: 200 with the status, 409 when
    /// another device owns it or the metadata changed, also once it is
    /// `complete`. The partial is reopened when it or the sidecar is gone (a
    /// sweep, a crash before the first chunk, a refusal), with the same
    /// receipt and an empty chunk set; a verified file waiting for a second
    /// intake attempt keeps its chunk set, so the phone's retry (announce,
    /// then complete) sends no chunk twice. 401 with nothing opened when the
    /// device was revoked since its receipt read. A receipt a `complete`
    /// admitted meanwhile stays `complete` ([`Engine::update`]), the answer
    /// says so, and the files this announce opened go.
    async fn reannounce(
        &self,
        mut receipt: HandoverReceipt,
        device: &PairedDevice,
        metadata: &RecordingMetadata,
    ) -> HandoverResponse {
        if receipt.device_id != device.id {
            return HandoverResponse::problem(
                StatusCode::CONFLICT,
                "another device owns this recording",
            );
        }
        let recording_id = receipt.recording_id;
        // Also for a `complete` receipt: the phone answered `complete` posts
        // `complete`, and that 200 deletes its copy. A different file under
        // an admitted id is refused instead, and stays on the phone.
        if receipt.byte_count != metadata.byte_count
            || receipt.sha256 != metadata.sha256
            || receipt.chunk_size != metadata.chunk_size
        {
            return HandoverResponse::problem(
                StatusCode::CONFLICT,
                "metadata differs from the first announcement",
            );
        }
        if receipt.state.kind() == HandoverStateKind::Complete {
            return HandoverResponse::json(StatusCode::OK, &Self::status_of(&receipt));
        }
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
        // The files `begin` just made go once the recording was admitted
        // meanwhile: a stale `complete` would find them and verify an empty
        // partial.
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
        // The write yielded: another chunk may have landed, the device may
        // have been revoked, or a `complete` may have admitted the
        // recording. Fold this chunk into the receipt as it stands now,
        // never into the copy from before the write.
        match self.add_chunk(recording_id, device.id, index).await {
            None => no_such_recording(),
            Some(Err(error)) => HandoverResponse::internal_error("saving the receipt", &error),
            Some(Ok(())) => HandoverResponse::empty(StatusCode::NO_CONTENT),
        }
    }

    /// `POST /v1/recordings/{id}/complete`: 200 `{meetingID}` once every
    /// chunk is present and the whole file hashes to the announced value;
    /// 409 with the status while chunks are missing or while an earlier
    /// `complete` is still verifying or admitting, and with no chunk listed
    /// when the partial went or was created again during the verify; 422 on
    /// a hash mismatch, after which the partial is gone and the phone starts
    /// over; 401 from the revoke of the device until it pairs again, and
    /// when a revoke landed during the receipt read or the verify of a
    /// recording not yet admitted.
    pub(super) async fn complete(
        &self,
        recording_id: Uuid,
        device: &PairedDevice,
    ) -> HandoverResponse {
        // `handle` refused a revoked device already; a revoke from another
        // thread may land in between, so this checks again under the lock
        // that takes the count. A `complete` that starts during the revoke's
        // store delete takes the count already bumped and may still read the
        // row, so the checks below would miss the revoke. The receipt's owner
        // is not known yet, so nothing is discarded.
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
            // Admitted before, so a revoke during the read admits nothing
            // new. A 401 would make the phone keep the recording, and its
            // upload after pairing again would become a second meeting.
            if self.revoked_since(device, revocation) {
                self.forget_own(recording_id, device.id);
            }
            return HandoverResponse::json(StatusCode::OK, &wire::CompleteResponse { meeting_id });
        }
        // A revoke during the read missed the receipt if it was only in the
        // store. Refuse before the `verifying` write, which would put its row
        // back once the phone paired again; `verified_file` checks again
        // after the hash.
        if let Some(refused) = self.refusal(recording_id, device, revocation) {
            return refused;
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
        match self
            .verified_file(&mut receipt, &metadata, device, revocation)
            .await
        {
            Verification::Answered(response) => response,
            Verification::File(file) => self.admit(&file, &metadata, device, &mut receipt).await,
        }
    }

    /// 401 when the device was revoked since `complete` took `revocation`,
    /// also by a revoke that a pairing has since cleared from `revoked`.
    /// That revoke may have missed the receipt, so its files are discarded
    /// and it leaves memory, unless memory holds another device's receipt
    /// of the recording id by then: another phone announced it after the
    /// revoke, and the files are that phone's
    /// ([`Engine::discard_and_forget_own`]).
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

    /// The verified file: the one already waiting after an earlier intake
    /// failure, else the partial once every chunk is present (409 with the
    /// status otherwise) and the whole file hashes to the announced value
    /// (422 and the partial is discarded otherwise), promoted to its final
    /// name. The partial stays open from before the `verifying` write to
    /// the promote, and a partial gone or replaced meanwhile answers 409
    /// with no chunk listed: the hash must be of the file the intake gets.
    /// 200 with the meeting when another `complete` admitted the recording
    /// since this one read the receipt. 401 when the device was revoked
    /// since `complete` took `revocation`; nothing yields between that check
    /// and the intake call: an admission past this check may still finish;
    /// the revoke's discard can also make it fail.
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
                // The state stays as memory holds it: a re-announce or a
                // chunk may have changed it since `complete` read it.
                let _ = self
                    .update(receipt, |edit| edit.received_chunks.clear())
                    .await;
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
        // Opened before the `verifying` write, the first yield. A partial
        // discarded and created again meanwhile (a stale `complete`'s
        // refusal, then the phone's announce) is another file, which
        // `Identity` tells apart.
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
        // A `complete` that read the receipt before another one admitted the
        // recording finds it `complete` now, and answers with its meeting.
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
        // Another thread may have replaced the partial between the check
        // above and the rename: the file moved must still be the one hashed.
        // A file moved in its place is not this upload's; it goes.
        if receiving_file::Identity::at(&promoted).ok() != Some(identity) {
            let _ = std::fs::remove_file(&promoted);
            return Verification::Answered(self.replaced_during_the_verify(receipt).await);
        }
        // Closed before the intake, which moves or deletes the file.
        drop(file);
        Verification::File(promoted)
    }

    /// The 401 of a `complete` whose device was revoked during the verify,
    /// without a write: once the phone paired again, one would put back the
    /// row the revoke deleted. The `verifying` write kept the receipt in
    /// memory, so the revoke discarded the files itself; a partial there
    /// now was created after that. While `revoked` holds the device it can
    /// only be the revoked phone's (an announce that passed the gate before
    /// the revoke), and it goes. Once the device paired again it is the new
    /// pairing's upload, and it stays; so does another phone's upload of the
    /// same recording id ([`Engine::discard_own_while_revoked`]).
    fn revoked_during_the_verify(
        &self,
        recording_id: Uuid,
        device: &PairedDevice,
    ) -> HandoverResponse {
        self.discard_own_while_revoked(recording_id, device.id);
        Self::unauthorized()
    }

    /// The 409 of a partial gone or created again during the verify: the
    /// chunk set is emptied, so the phone sends every chunk again into the
    /// partial there now. Swift answers the same.
    async fn replaced_during_the_verify(&self, receipt: &mut HandoverReceipt) -> HandoverResponse {
        let _ = self
            .transition(receipt, HandoverState::Receiving, Some(Vec::new()))
            .await;
        HandoverResponse::json(StatusCode::CONFLICT, &Self::status_of(receipt))
    }

    /// Hands the verified file to the intake. On success the receipt is
    /// `complete` and the answer is 200 whatever the receipt write did: the
    /// real intake wrote this same receipt and deleted the file. Then every
    /// file of the recording goes: the verified file if the intake left it,
    /// the metadata sidecar, and a partial a re-announce opened during the
    /// intake. A replayed complete returns the same id
    /// through the early `complete` check. When another device announced
    /// the same recording id meanwhile (this one was revoked during the
    /// intake), only the verified file goes, and the rest is that phone's
    /// upload ([`Engine::discard_own`]); no other request creates the
    /// verified file while this `complete` holds the `completing` mark. On
    /// failure the verified file stays for the phone's retry and the reason
    /// is fixed text, because the error may name the file's path.
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

#[cfg(test)]
mod tests {
    //! White-box checks of what a request on another worker thread can
    //! leave in the synchronous stretches no gate of the integration tests
    //! can stop: the state is built by hand, and the step after the
    //! stretch is called as its route calls it.

    use std::sync::Arc;

    use chrono::{TimeZone as _, Utc};
    use steno_core::testing::FakeHandoverIntake;
    use steno_core::{AudioFormat, PairedDevice, RecordingMetadata, Store};
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

    #[tokio::test]
    async fn a_refusal_leaves_another_devices_receipt_and_files_alone() {
        // X's revoke landed during the store read of phone X's `complete`
        // and bumped the count, so the read did not remember X's receipt.
        // In the stretch after the read, phone Y announced the same
        // recording id on another thread, which made its receipt and opened
        // its files. The refusal then leaves Y's upload alone.
        let directory = tempfile::tempdir().unwrap();
        let now = Utc.timestamp_opt(1_790_000_000, 0).unwrap();
        let configuration = HandoverConfiguration {
            service_name: "Test".to_owned(),
            advertise: false,
            inbox_directory: directory.path().join("inbox"),
            ..HandoverConfiguration::default()
        };
        let store = Arc::new(Store::in_memory().unwrap());
        let x = device("X");
        let y = device("Y");
        store.save_paired_device(&x, &[1; 32]).unwrap();
        store.save_paired_device(&y, &[2; 32]).unwrap();
        let engine = Engine::new(
            configuration,
            Arc::new(HandoverIdentity::mint("Test", now).unwrap()),
            store,
            Arc::new(FakeHandoverIntake::default()),
            watch::channel(Vec::new()).0,
            Arc::new(move || now),
        );

        // The count X's `complete` takes before its receipt read.
        let revocation = engine.state().revocation_count(x.id);
        engine.revoke(x.id).await.unwrap();
        let recording_id = Uuid::new_v4();
        let metadata = RecordingMetadata {
            recording_id,
            started_at: Utc.timestamp_opt(1_789_990_000, 0).unwrap(),
            duration_seconds: 1.0,
            byte_count: 3,
            sha256: vec![7; 32],
            chunk_size: 64 * 1024,
            format: AudioFormat::M4aAac,
            device_name: "Y".to_owned(),
        };
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
}
