use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use second::{
    AccountView, LegalTaskStatus, NodeRuntime, PublicStateStore, QuicClient, QuicPeer,
    QuicTransportIdentity,
};

use super::storage::WalletSession;
use super::{StoredRequest, WalletData};

pub(super) struct WalletNode {
    pub runtime: Arc<NodeRuntime>,
    identity: QuicTransportIdentity,
    listener: tokio::task::JoinHandle<Result<(), second::NodeRuntimeError>>,
}

impl Drop for WalletNode {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

impl WalletNode {
    pub fn start(session: &WalletSession) -> Result<Self, String> {
        let base = session.directory.join("node");
        let runtime = Arc::new(
            NodeRuntime::load_public_and_bind(
                SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                &PublicStateStore::new(&base),
            )
            .map_err(|error| format!("cannot start wallet node: {error:?}"))?,
        );
        let identity =
            QuicTransportIdentity::load_or_generate(second::transport_identity_path(&base))
                .map_err(|error| format!("wallet identity: {error:?}"))?;
        let running = Arc::clone(&runtime);
        let listener = tokio::spawn(async move { running.run(&[]).await });
        Ok(Self {
            runtime,
            identity,
            listener,
        })
    }

    async fn connect(
        &self,
        data: &WalletData,
        start: usize,
    ) -> Result<(QuicClient, QuicPeer), String> {
        let mut failures = Vec::new();
        for offset in 0..data.network.endpoints.len() {
            let endpoint = &data.network.endpoints[(start + offset) % data.network.endpoints.len()];
            let cert = base64::engine::general_purpose::STANDARD
                .decode(&endpoint.certificate_base64)
                .map_err(|_| "invalid certificate")?;
            let address = crate::parse_socket_address(&endpoint.address)?;
            let bind = if address.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            }
            .parse()
            .unwrap();
            let client = QuicClient::new(bind, &cert, self.identity.clone())
                .map_err(|error| format!("wallet client: {error:?}"))?;
            match tokio::time::timeout(Duration::from_secs(3), client.connect(address)).await {
                Ok(Ok(peer)) => return Ok((client, peer)),
                other => failures.push(format!("{}: {other:?}", endpoint.address)),
            }
        }
        Err(format!(
            "no wallet endpoint reachable: {}",
            failures.join("; ")
        ))
    }

    pub async fn query(&self, data: &WalletData, kind: u8) -> Result<AccountView, String> {
        let key = data.account_key()?;
        let mut last = String::new();
        for attempt in 0..3 {
            let (client, peer) = self.connect(data, attempt).await?;
            let result = second::client_account_view(&peer, &key, kind)
                .await
                .map_err(|error| format!("account query: {error:?}"));
            peer.close();
            client.wait_idle().await;
            match result {
                Ok(view) => return Ok(view),
                Err(error) => last = error,
            }
        }
        Err(last)
    }

    pub async fn status(
        &self,
        data: &WalletData,
        request: &StoredRequest,
    ) -> Result<LegalTaskStatus, String> {
        let task = super::commands::signed_task(request)?
            .ok_or("request is awaiting Authorizer signature")?;
        let mut last = String::new();
        for attempt in 0..data.network.endpoints.len() {
            let (client, peer) = self.connect(data, attempt).await?;
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                second::client_legal_task_status(&peer, &task),
            )
            .await;
            peer.close();
            client.wait_idle().await;
            match result {
                Ok(Ok(remote)) if remote.status != LegalTaskStatus::Unknown => {
                    return Ok(remote.status);
                }
                Ok(Ok(_)) => last = "unknown".to_owned(),
                other => last = format!("status unavailable: {other:?}"),
            }
        }
        if last == "unknown" {
            Ok(LegalTaskStatus::Unknown)
        } else {
            Err(last)
        }
    }

    pub async fn submit(
        &self,
        data: &WalletData,
        request: &StoredRequest,
        wait: bool,
    ) -> Result<LegalTaskStatus, String> {
        let task = super::commands::signed_task(request)?
            .ok_or("request is awaiting Authorizer signature; use export-request and authorize")?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(if wait { 30 } else { 8 });
        let mut last = String::new();
        for attempt in 0..data.network.endpoints.len() {
            let (client, peer) = match self.connect(data, attempt).await {
                Ok(peer) => peer,
                Err(error) => {
                    last = error;
                    continue;
                }
            };
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                second::client_submit_legal_task(&peer, &task),
            )
            .await;
            // The validator serves one private submission or status request
            // per authenticated QUIC connection. Polling on the submitted
            // connection races its intentional server-side close.
            peer.close();
            client.wait_idle().await;
            if let Ok(Ok(_)) = result {
                loop {
                    match tokio::time::timeout(Duration::from_secs(5), self.status(data, request))
                        .await
                    {
                        Ok(Ok(status)) => {
                            if !wait
                                || matches!(
                                    status,
                                    LegalTaskStatus::Succeeded | LegalTaskStatus::Cancelled
                                )
                            {
                                return Ok(status);
                            }
                            last = format!("{status:?}");
                        }
                        other => {
                            last = format!("status response lost: {other:?}");
                            break;
                        }
                    }
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
            } else {
                last = format!("submission response: {result:?}");
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        Err(format!(
            "request {} remains saved; query or retry this same task: {last}",
            task.payload().task_id()
        ))
    }

    pub async fn sync(&self, data: &WalletData) -> Result<(), String> {
        let (client, peer) = self.connect(data, 0).await?;
        let records = second::client_peer_records(&peer, 1)
            .await
            .map_err(|error| format!("wallet peer discovery: {error:?}"))?;
        peer.close();
        client.wait_idle().await;
        self.runtime
            .bootstrap(&records, records.len())
            .await
            .map_err(|error| format!("wallet bootstrap: {error:?}"))?;
        self.runtime
            .sync_freshest_certified_public_currency_view()
            .await
            .map_err(|error| format!("wallet public sync: {error:?}"))?;
        Ok(())
    }
}
