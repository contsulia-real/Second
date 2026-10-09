//! One bounded projection per private query connection; no snapshot survives construction.
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use super::*;
use crate::PersistedNodeState;
use crate::network::{NodeId, QuicPeer, QuicRequestStream};

const MAX_PROJECTION_BYTES: usize = 16 * 1024 * 1024;
const MAX_ACTIVE_PROJECTIONS: usize = 4;
mod projection;
use projection::Projection;

static ACTIVE_PROJECTIONS: AtomicUsize = AtomicUsize::new(0);
type SnapshotLoader = Arc<dyn Fn() -> Result<Arc<PersistedNodeState>, NetworkError> + Send + Sync>;

struct Permit;
impl Permit {
    fn acquire() -> Result<Self, NetworkError> {
        ACTIVE_PROJECTIONS
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
                (active < MAX_ACTIVE_PROJECTIONS).then_some(active + 1)
            })
            .map(|_| Self)
            .map_err(|_| denied("busy"))
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        ACTIVE_PROJECTIONS.fetch_sub(1, Ordering::Relaxed);
    }
}
struct Cancellation(Arc<AtomicBool>);
impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

fn denied(reason: &str) -> NetworkError {
    NetworkError::AccountQueryDenied(reason.to_owned())
}

fn check_work(deadline: Instant, cancelled: &AtomicBool) -> Result<(), NetworkError> {
    if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
        return Err(denied("session_expired"));
    }
    Ok(())
}

async fn respond_before(
    request: QuicRequestStream,
    response: &NetworkMessage,
    deadline: Instant,
) -> Result<(), NetworkError> {
    tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        request.respond(response),
    )
    .await
    .map_err(|_| denied("session_expired"))?
}

pub(in crate::network) async fn serve(
    peer: &QuicPeer,
    first: QuicRequestStream,
    loader: Option<SnapshotLoader>,
) -> Result<NodeId, NetworkError> {
    serve_until(peer, first, loader, Instant::now() + ACCOUNT_QUERY_LIFETIME).await
}

async fn serve_until(
    peer: &QuicPeer,
    mut request: QuicRequestStream,
    loader: Option<SnapshotLoader>,
    deadline: Instant,
) -> Result<NodeId, NetworkError> {
    let cancellation = Cancellation(Arc::new(AtomicBool::new(false)));
    let cancelled = Arc::clone(&cancellation.0);
    let binding = peer.channel_binding()?;
    let message = request.message().clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        Projection::build(binding, &message, loader, deadline, &cancelled)
    });
    let built = tokio::select! {
        result = &mut worker => result.map_err(|error| NetworkError::Transport(format!("account query worker failed: {error}")))?,
        _ = peer.closed() => return Ok(peer.remote_node_id()),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => return Err(denied("session_expired")),
    };
    let mut projection = match built {
        Ok(value) => value,
        Err(NetworkError::AccountQueryDenied(reason)) => {
            respond_before(request, &rejected(&reason), deadline).await?;
            return Ok(peer.remote_node_id());
        }
        Err(error) => return Err(error),
    };
    loop {
        check_work(deadline, &cancellation.0)?;
        let view = match projection.page_authenticated(request.message()) {
            Ok(view) => view,
            Err(NetworkError::AccountQueryDenied(reason)) => {
                respond_before(request, &rejected(&reason), deadline).await?;
                return Ok(peer.remote_node_id());
            }
            Err(error) => return Err(error),
        };
        let finished = view.next.is_none();
        respond_before(
            request,
            &NetworkMessage::AccountQueryResult { view },
            deadline,
        )
        .await?;
        if finished {
            return Ok(peer.remote_node_id());
        }
        let idle_deadline = (Instant::now() + Duration::from_secs(5)).min(deadline);
        match tokio::time::timeout_at(
            tokio::time::Instant::from_std(idle_deadline),
            peer.accept_request(),
        )
        .await
        {
            Ok(Ok(Some(next))) => {
                if authenticate(binding, next.message()).is_err() {
                    respond_before(next, &rejected("unauthorized"), deadline).await?;
                    return Ok(peer.remote_node_id());
                }
                request = next;
            }
            Ok(Ok(None)) => return Ok(peer.remote_node_id()),
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(denied("session_expired")),
        }
    }
}

#[cfg(test)]
mod tests;
