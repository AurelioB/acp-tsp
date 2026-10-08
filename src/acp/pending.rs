//! Pending user decisions. IDs are process-global so replies from before a restart cannot match.
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use agent_client_protocol::schema::v1::{
    CreateElicitationRequest, CreateElicitationResponse, ElicitationAction,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
};
use agent_client_protocol::{Error, Responder};

use crate::bus::EventTx;
use crate::event::{BridgeEvent, PendingId};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

enum Decision {
    Permission(Responder<RequestPermissionResponse>),
    Elicitation(Responder<CreateElicitationResponse>),
}

struct Pending {
    responder: Decision,
    done: tokio::sync::oneshot::Sender<()>,
}

#[derive(Default)]
pub(super) struct Registry {
    entries: Mutex<BTreeMap<PendingId, Pending>>,
    /// A request racing session/cancel must be cancelled before it reaches the UI.
    cancelling: AtomicBool,
}

impl Registry {
    pub fn insert_permission(
        &self,
        responder: Responder<RequestPermissionResponse>,
        request: RequestPermissionRequest,
        events: &EventTx,
    ) -> Result<Option<(PendingId, tokio::sync::oneshot::Receiver<()>)>, Error> {
        let mut entries = self.entries.lock();
        if self.cancelling.load(Ordering::SeqCst) {
            responder.respond(RequestPermissionResponse::new(
                RequestPermissionOutcome::Cancelled,
            ))?;
            return Ok(None);
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (done, closed) = tokio::sync::oneshot::channel();
        entries.insert(
            id,
            Pending {
                responder: Decision::Permission(responder),
                done,
            },
        );
        events.send(BridgeEvent::Permission {
            id,
            request: Box::new(request),
        });
        Ok(Some((id, closed)))
    }

    pub fn insert_elicitation(
        &self,
        responder: Responder<CreateElicitationResponse>,
        request: CreateElicitationRequest,
        events: &EventTx,
    ) -> Result<Option<(PendingId, tokio::sync::oneshot::Receiver<()>)>, Error> {
        let mut entries = self.entries.lock();
        if self.cancelling.load(Ordering::SeqCst) {
            responder.respond(CreateElicitationResponse::new(ElicitationAction::Cancel))?;
            return Ok(None);
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (done, closed) = tokio::sync::oneshot::channel();
        entries.insert(
            id,
            Pending {
                responder: Decision::Elicitation(responder),
                done,
            },
        );
        events.send(BridgeEvent::Elicitation {
            id,
            request: Box::new(request),
        });
        Ok(Some((id, closed)))
    }

    pub fn answer_permission(
        &self,
        id: PendingId,
        outcome: Result<RequestPermissionOutcome, Error>,
    ) -> bool {
        let mut entries = self.entries.lock();
        if !matches!(entries.get(&id).map(|entry| &entry.responder), Some(Decision::Permission(_))) {
            return false;
        }
        let pending = entries.remove(&id).expect("checked above");
        drop(entries);
        let _ = pending.done.send(());
        if let Decision::Permission(responder) = pending.responder {
            let _ = responder.respond_with_result(outcome.map(RequestPermissionResponse::new));
        }
        true
    }

    pub fn answer_elicitation(
        &self,
        id: PendingId,
        response: Result<CreateElicitationResponse, Error>,
    ) -> bool {
        let mut entries = self.entries.lock();
        if !matches!(entries.get(&id).map(|entry| &entry.responder), Some(Decision::Elicitation(_))) {
            return false;
        }
        let pending = entries.remove(&id).expect("checked above");
        drop(entries);
        let _ = pending.done.send(());
        if let Decision::Elicitation(responder) = pending.responder {
            let _ = responder.respond_with_result(response);
        }
        true
    }

    pub fn cancel_turn(&self, events: &EventTx) {
        // Keep the lock across the flag update and drain: insertion sees either state atomically.
        let mut entries = self.entries.lock();
        self.cancelling.store(true, Ordering::SeqCst);
        drain(&mut entries, events);
    }

    pub fn reset(&self, events: &EventTx) {
        let mut entries = self.entries.lock();
        drain(&mut entries, events);
        self.cancelling.store(false, Ordering::SeqCst);
    }

    pub fn turn_ended(&self) {
        self.cancelling.store(false, Ordering::SeqCst);
    }
}

fn drain(entries: &mut BTreeMap<PendingId, Pending>, events: &EventTx) {
    for (id, pending) in std::mem::take(entries) {
        let _ = pending.done.send(());
        match pending.responder {
            Decision::Permission(responder) => {
                let _ = responder.respond(RequestPermissionResponse::new(
                    RequestPermissionOutcome::Cancelled,
                ));
            }
            Decision::Elicitation(responder) => {
                let _ =
                    responder.respond(CreateElicitationResponse::new(ElicitationAction::Cancel));
            }
        }
        events.send(BridgeEvent::Resolved(id));
    }
}
