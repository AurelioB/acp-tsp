//! Agent → client request implementations. Never block the SDK dispatch loop on a request.
use std::sync::Arc;

use agent_client_protocol::schema::v1::{
    CreateElicitationRequest, CreateElicitationResponse, ElicitationMode, ElicitationScope,
    ReadTextFileRequest, ReadTextFileResponse, RequestPermissionRequest, RequestPermissionResponse,
    SessionNotification, WriteTextFileRequest, WriteTextFileResponse,
};
use agent_client_protocol::{Agent, ConnectionTo, Error, Responder};

use super::Shared;
use crate::bus::EventTx;
use crate::event::BridgeEvent;
use crate::host::fs::FileSystem;

pub(super) async fn update(
    request: SessionNotification,
    shared: &Shared,
    events: &EventTx,
) -> Result<(), Error> {
    if shared.active(&request.session_id) {
        events.send(BridgeEvent::Update(Box::new(request.update)));
    } else {
        tracing::debug!("ignoring update for inactive session");
    }
    Ok(())
}

pub(super) fn permission(
    request: RequestPermissionRequest,
    responder: Responder<RequestPermissionResponse>,
    cx: ConnectionTo<Agent>,
    shared: Arc<Shared>,
    events: EventTx,
) -> Result<(), Error> {
    if !shared.active(&request.session_id) {
        return responder.respond_with_error(Error::invalid_params());
    }
    let cancellation = responder.cancellation();
    let Some((id, closed)) = shared
        .pending
        .insert_permission(responder, request, &events)?
    else {
        return Ok(());
    };
    cx.spawn(async move {
        tokio::select! {
            _ = cancellation.cancelled() => {
                if shared.pending.answer_permission(id, Err(Error::request_cancelled())) { events.send(BridgeEvent::Resolved(id)); }
            }
            _ = closed => {},
        }
        Ok(())
    })
}

pub(super) fn elicitation(
    request: CreateElicitationRequest,
    responder: Responder<CreateElicitationResponse>,
    cx: ConnectionTo<Agent>,
    shared: Arc<Shared>,
    events: EventTx,
) -> Result<(), Error> {
    match request.scope() {
        ElicitationScope::Session(scope) if !shared.active(&scope.session_id) => {
            return responder.respond_with_error(Error::invalid_params());
        }
        ElicitationScope::Request(_) => {}
        ElicitationScope::Session(_) => {}
        _ => return responder.respond_with_error(Error::invalid_params()),
    }
    if !matches!(
        request.mode,
        ElicitationMode::Form(_) | ElicitationMode::Url(_)
    ) {
        return responder.respond_with_error(Error::invalid_params());
    }
    let cancellation = responder.cancellation();
    let Some((id, closed)) = shared
        .pending
        .insert_elicitation(responder, request, &events)?
    else {
        return Ok(());
    };
    cx.spawn(async move {
        tokio::select! {
            _ = cancellation.cancelled() => {
                if shared.pending.answer_elicitation(id, Err(Error::request_cancelled())) { events.send(BridgeEvent::Resolved(id)); }
            }
            _ = closed => {},
        }
        Ok(())
    })
}

pub(super) fn read(
    request: ReadTextFileRequest,
    responder: Responder<ReadTextFileResponse>,
    cx: ConnectionTo<Agent>,
    shared: &Shared,
    fs: FileSystem,
) -> Result<(), Error> {
    if !shared.active(&request.session_id) {
        return responder.respond_with_error(Error::invalid_params());
    }
    let cancellation = responder.cancellation();
    cx.spawn(async move {
        let result = cancellation
            .run_until_cancelled(async move {
                tokio::task::spawn_blocking(move || fs.read(request))
                    .await
                    .map_err(Error::into_internal_error)?
            })
            .await;
        responder.respond_with_result(result)
    })
}

pub(super) fn write(
    request: WriteTextFileRequest,
    responder: Responder<WriteTextFileResponse>,
    cx: ConnectionTo<Agent>,
    shared: &Shared,
    fs: FileSystem,
) -> Result<(), Error> {
    if !shared.active(&request.session_id) {
        return responder.respond_with_error(Error::invalid_params());
    }
    let cancellation = responder.cancellation();
    cx.spawn(async move {
        let result = cancellation
            .run_until_cancelled(async move {
                tokio::task::spawn_blocking(move || fs.write(request))
                    .await
                    .map_err(Error::into_internal_error)?
            })
            .await;
        responder.respond_with_result(result)
    })
}
