//! Test deadlines distinguish continuing work from a stalled runtime.
use std::{fmt::Debug, future::Future, time::Duration};
use tokio::time::Instant;

const OVERALL_BUDGET: Duration = Duration::from_secs(60);

pub async fn snapshots(
    stores: impl IntoIterator<Item = second::StateStore>,
) -> Vec<second::PersistedNodeState> {
    let stores = stores.into_iter().collect::<Vec<_>>();
    tokio::task::spawn_blocking(move || {
        stores
            .iter()
            .map(|store| store.load().unwrap().unwrap())
            .collect()
    })
    .await
    .unwrap()
}

pub async fn snapshot(store: &second::StateStore) -> second::PersistedNodeState {
    snapshots([store.clone()]).await.pop().unwrap()
}

pub async fn public_snapshot(store: &second::PublicStateStore) -> second::PersistedPublicNodeState {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.load().unwrap().unwrap())
        .await
        .unwrap()
}

pub(crate) struct ProgressDeadline<T> {
    end: Instant,
    idle_budget: Duration,
    changed: Option<Instant>,
    last: Option<T>,
}

impl<T: Eq + Debug> ProgressDeadline<T> {
    pub(crate) fn new(now: Instant, idle_budget: Duration, overall: Duration) -> Self {
        Self {
            end: now + overall,
            idle_budget,
            changed: None,
            last: None,
        }
    }

    pub(crate) fn observe(&mut self, now: Instant, value: T) -> Result<(), String> {
        if now >= self.end {
            return Err(format!("overall deadline reached; last={:?}", self.last));
        }
        if self.last.as_ref() != Some(&value) {
            self.last = Some(value);
            self.changed = Some(now);
        } else if self
            .changed
            .is_some_and(|changed| now.duration_since(changed) >= self.idle_budget)
        {
            return Err(format!(
                "no progress for {:?}; last={:?}",
                self.idle_budget, self.last
            ));
        }
        Ok(())
    }
}

pub async fn wait_for_progress<T, F, Fut>(idle_budget: Duration, mut probe: F) -> Result<(), String>
where
    T: Eq + Debug,
    F: FnMut() -> Fut,
    Fut: Future<Output = (bool, T)>,
{
    let start = Instant::now();
    let mut deadline = ProgressDeadline::new(start, idle_budget, OVERALL_BUDGET);
    tokio::time::timeout_at(start + OVERALL_BUDGET, async {
        loop {
            let (done, progress) = probe().await;
            deadline.observe(Instant::now(), progress)?;
            if done {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .map_err(|_| format!("overall deadline reached; last={:?}", deadline.last))?
}
