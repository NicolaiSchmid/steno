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

/// The receipt an announce made, the save's place in line, and what
/// [`Engine::open_files`] returned.
type MadeAndOpened = (HandoverReceipt, super::InOrder, Option<std::io::Result<()>>);

/// Where one decision of an announce ended.
enum Announced {
    Answered(HandoverResponse),
    /// Memory held another receipt of the recording id by the time of the
    /// change than the one the decision was made on; the announce decides
    /// again with that one.
    Changed(Box<HandoverReceipt>),
}

pub(super) fn no_such_recording() -> HandoverResponse {
    HandoverResponse::problem(StatusCode::NOT_FOUND, "no such recording")
}

impl Engine {
    /// `PUT /v1/recordings/{id}` with `RecordingMetadata`: 201 for a new
    /// recording (no receipt, or other bytes than the receipt's), 200 for a
    /// known one, both with `RecordingStatus`; 200 `complete` with every
    /// chunk listed for bytes the admission ledger shows admitted; 409 only
    /// for admitted bytes over another device's unfinished upload of other
    /// bytes; 401 when the device was revoked since its receipt read. The
    /// decision table is in `.plans/2026-10-08-handover-admission-ledger.md`.
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
        // A first announce of the same recording on another thread may have
        // made its receipt since the read above, and a chunk may have landed
        // in it; a replacement may meet a receipt changed since the read, or
        // since the ledger read it waited for. Then the receipt memory holds
        // stays, and this announce decides again as if the read had found it.
        loop {
            let announced = match existing {
                None => self.first_announce(device, &metadata).await,
                Some(existing) => self.reannounce(existing, device, &metadata).await,
            };
            match announced {
                Announced::Answered(response) => return response,
                Announced::Changed(held) => existing = Some(*held),
            }
        }
    }

    /// The meeting the admission ledger holds for `metadata`'s recording
    /// id, size and SHA-256 ([`steno_core::Store::admitted_meeting`]), or
    /// the 500 of a failed read. Read only where it decides something: no
    /// receipt, or one of other bytes. Its rows are never deleted, so a row
    /// read stays true; an admission that commits after the read belongs to
    /// a receipt memory then holds, which the replacement's check in
    /// [`Engine::make_and_open`] finds.
    async fn admitted(&self, metadata: &RecordingMetadata) -> Result<Option<Uuid>, Announced> {
        let (recording_id, byte_count, sha256) = (
            metadata.recording_id,
            metadata.byte_count,
            metadata.sha256.clone(),
        );
        self.with_store(move |store| store.admitted_meeting(recording_id, byte_count, &sha256))
            .await
            .map_err(|error| {
                Announced::Answered(HandoverResponse::internal_error(
                    "reading the admissions",
                    &error,
                ))
            })
    }

    /// An announce whose read found no receipt in memory or the store: a
    /// `complete` receipt with the ledger's meeting id when the ledger
    /// holds these bytes (200, every chunk listed, nothing opened: the
    /// phone posts `complete`, takes the meeting id and deletes its copy),
    /// else a new recording (201).
    async fn first_announce(
        &self,
        device: &PairedDevice,
        metadata: &RecordingMetadata,
    ) -> Announced {
        let admitted = match self.admitted(metadata).await {
            Ok(admitted) => admitted,
            Err(failed) => return failed,
        };
        if let Some(meeting_id) = admitted {
            let delivered = self.fresh(device, metadata, HandoverState::Complete { meeting_id });
            return self.replace(None, delivered, None, StatusCode::OK).await;
        }
        let fresh = self.fresh(device, metadata, HandoverState::Receiving);
        self.replace(None, fresh, Some(metadata), StatusCode::CREATED)
            .await
    }

    /// A receipt of `metadata`'s bytes for `device` in `state`, no chunk
    /// received, made now.
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

    /// Makes `fresh` the receipt of its recording id in place of
    /// `expected`, the one the announce read (`None`: it read none), opens
    /// its files (`begin`; none for a receipt answered `complete`) and
    /// answers `status` with it. [`Announced::Changed`] when memory holds
    /// another receipt by then.
    ///
    /// The place in line goes to the save whatever the opening did: nothing
    /// between `change` and the save yields. A failed opening leaves the
    /// receipt saved without files, so the phone's retried announce reopens
    /// them as a re-announce. A device revoked since its receipt read opens
    /// none and is answered 401: its receipt stayed out of memory, or left
    /// it. That receipt is still saved, so a device revoked and paired
    /// again since leaves a row without files, and its retried announce
    /// reopens them as a re-announce.
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
    /// `fresh` the receipt memory holds while memory holds `expected` or
    /// none, or declines with the one memory holds by then, and
    /// [`Engine::open_files`] discards the recording id's files and opens
    /// those of the receipt it made. One hold of the lock covers both, so
    /// no other request creates or discards a file of the recording in
    /// between, and the sidecar is the metadata of the receipt memory
    /// holds. Nothing in it yields, and the lock is released before the
    /// save. The unit tests check the lock is held at `change` and run
    /// their in-between step where requests on other threads that take no
    /// files lock could.
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

    /// A known recording announced again; the rows of the decision table
    /// with a receipt. Other bytes than the receipt's: a `complete` receipt
    /// with the ledger's meeting id when the ledger holds them (as for no
    /// receipt) and the receipt is `complete` or this device's; 409 over
    /// another device's unfinished upload, which a `complete` answer would
    /// end with that phone deleting its copy; else a new recording under
    /// the same recording id (201), whose own `complete` admits a meeting
    /// of its own. The same bytes from another device take the receipt over
    /// ([`Engine::take_over`]). Then, as the owner: a `complete` receipt
    /// answers 200 with every chunk of the announced split; one in another
    /// split starts its partial over under the announced one (200, no chunk
    /// listed); else [`Engine::resume`].
    async fn reannounce(
        &self,
        receipt: HandoverReceipt,
        device: &PairedDevice,
        metadata: &RecordingMetadata,
    ) -> Announced {
        let complete = receipt.state.kind() == HandoverStateKind::Complete;
        if receipt.byte_count != metadata.byte_count || receipt.sha256 != metadata.sha256 {
            let admitted = match self.admitted(metadata).await {
                Ok(admitted) => admitted,
                Err(failed) => return failed,
            };
            return match admitted {
                Some(meeting_id) if complete || receipt.device_id == device.id => {
                    let delivered =
                        self.fresh(device, metadata, HandoverState::Complete { meeting_id });
                    self.replace(Some(&receipt), delivered, None, StatusCode::OK)
                        .await
                }
                Some(_) => Announced::Answered(HandoverResponse::problem(
                    StatusCode::CONFLICT,
                    "another device owns this recording",
                )),
                None => {
                    let fresh = self.fresh(device, metadata, HandoverState::Receiving);
                    self.replace(Some(&receipt), fresh, Some(metadata), StatusCode::CREATED)
                        .await
                }
            };
        }
        let receipt = if receipt.device_id == device.id {
            receipt
        } else {
            match self.take_over(&receipt, device).await {
                Ok(taken) => taken,
                Err(announced) => return announced,
            }
        };
        // The chunk size matters only until the receipt is `complete`: the
        // same bytes split otherwise are the file the computer holds. The
        // answer lists every chunk of the phone's split, so it posts
        // `complete`; the copy it is built from is never saved.
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

    /// The receipt of another device announced with the same size and
    /// SHA-256 becomes `device`'s, its chunks and files kept: the same
    /// bytes are the same recording, so whichever device's `complete`
    /// admits them, the other phone's copy is that recording, and its next
    /// announce takes the receipt back as `complete` and is answered
    /// delivered. The older device's requests then find no receipt of
    /// theirs (404), and its late writes leave this one alone
    /// ([`Engine::update`]). [`Announced::Changed`] when memory holds
    /// another receipt by then; a failed save is answered 500, with memory
    /// holding the receipt as taken over.
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

    /// The owner's re-announce of the same bytes in the same split: 200
    /// with the status. The partial is reopened when it or the sidecar is
    /// gone (a sweep, a crash before the first chunk, a refusal), with the
    /// same receipt and an empty chunk set; a verified file waiting for a
    /// second intake attempt keeps its chunk set, so the phone's retry
    /// (announce, then complete) sends no chunk twice. 401 with nothing
    /// opened when the device was revoked since its receipt read. A receipt
    /// a `complete` admitted meanwhile stays `complete` ([`Engine::update`]),
    /// the answer says so, and the files this announce opened go.
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
        match self.add_chunk(&receipt, index).await {
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
    /// is fixed text, because the error may name the file's path; unless
    /// memory holds a receipt of other bytes by then (the phone announced
    /// another file under the id during the intake): that upload's
    /// `complete` would hand this file to the intake unhashed
    /// ([`Engine::verified_file`]), so it goes.
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

#[cfg(test)]
mod tests {
    //! White-box checks of what a request on another worker thread can
    //! leave in the synchronous stretches no gate of the integration tests
    //! can stop: the state is built by hand, and the step after the
    //! stretch is called as its route calls it, or the route runs with the
    //! test's edit inside the stretch ([`first_announce`]).

    use std::cell::RefCell;
    use std::sync::Arc;

    use chrono::{DateTime, TimeZone as _, Utc};
    use steno_core::testing::FakeHandoverIntake;
    use steno_core::{AudioFormat, HandoverReceipt, PairedDevice, RecordingMetadata, Store};
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

    fn engine(directory: &std::path::Path, store: Arc<Store>, now: DateTime<Utc>) -> Engine {
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

    #[tokio::test]
    async fn a_refusal_leaves_another_devices_receipt_and_files_alone() {
        // X's revoke landed during the store read of phone X's `complete`
        // and bumped the count, so the read did not remember X's receipt.
        // In the stretch after the read, phone Y announced the same
        // recording id on another thread, which made its receipt and opened
        // its files. The refusal then leaves Y's upload alone.
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
        // A chunk request read the receipt and wrote its bytes at an offset
        // of that receipt's split. Before it folds its chunk in, the phone
        // announced the same bytes in another split (the partial restarts)
        // or another file under the id (a new recording). The chunk is not
        // the new upload's: the fold leaves its receipt alone, so the phone
        // sends that chunk again.
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

    /// An edit of the receipt memory holds, as a request on another thread
    /// makes it.
    type Edit = Box<dyn FnOnce(&mut HandoverReceipt)>;

    thread_local! {
        /// The edit the next first announce on this thread runs between
        /// its `change` and its opening.
        static MEANWHILE: RefCell<Option<Edit>> = const { RefCell::new(None) };
    }

    /// Runs the edit [`first_announce`] set for this thread, if any; called
    /// by `Engine::make_and_open` between its `change` and its opening.
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
    /// `meanwhile` editing the receipt memory holds between its `change`
    /// and its opening, as requests on other threads that take no files
    /// lock would.
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
        // Phone X's partial is on disk and memory holds no receipt of it
        // (a restart). X's revoke finds nothing in memory to discard, and
        // its delete takes the row. Phone Y then announces the same
        // recording id: its read finds nothing, and its announce discards
        // X's partial before `begin`, so Y's chunks never land in X's
        // bytes. X's `complete` from before the revoke is then refused and
        // leaves Y's files alone.
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
        // Phone X's upload of the recording id is under way after its
        // revoke and pairing again: its partial and sidecar are on disk.
        // Phone Y's first announce read nothing (X's receipt was out of
        // memory and the store), took the files lock and made its receipt
        // at its `change`. Before its opening, on other threads that take
        // no files lock: Y is revoked, which drops Y's receipt from memory,
        // and pairs again, and a request of X's writes X's receipt back.
        // Y's opening then neither discards X's files nor writes Y's
        // sidecar over X's.
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
        // A verified file waits after an intake failure whose receipt a
        // revoke deleted after a restart. Phone Y's first announce read
        // nothing, took the files lock and made its receipt at its
        // `change`. Before its opening, a chunk request of Y's from before
        // an earlier revoke and pairing again folds its chunk into that
        // receipt; it takes no files lock. Y's opening still discards the
        // verified file: kept, Y's `complete` would admit it unhashed and
        // Y's phone would delete its own recording. The chunk listed in
        // the receipt but missing from the empty partial fails the hash, so
        // Y's phone starts over.
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
}
