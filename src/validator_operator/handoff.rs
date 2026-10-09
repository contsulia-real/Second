//! Operator entry for authenticated, signing-locked member initialization.
use super::*;

pub(crate) async fn install_handoff(
    address: &str,
    destination_snapshot_base: &str,
    trust_snapshot_base: &str,
    server_certificate: &str,
) -> Result<(), String> {
    let _directory_lock =
        crate::local_file::lock_node_directory(Path::new(destination_snapshot_base))?;
    let destination = StateStore::new(destination_snapshot_base);
    if destination
        .load()
        .map_err(|error| format!("failed to inspect destination snapshot: {error:?}"))?
        .is_some()
    {
        return Err(format!(
            "destination snapshot {destination_snapshot_base} is already initialized"
        ));
    }
    let trusted = StateStore::new(trust_snapshot_base)
        .load()
        .map_err(|error| format!("failed to load trust snapshot: {error:?}"))?
        .ok_or_else(|| format!("no trust snapshot found at {trust_snapshot_base}"))?;
    let material = crate::validator_keyring::read_material(
        &crate::validator_keyring::keyring_path(Path::new(destination_snapshot_base)),
    )?;
    let (authorizers, _) = crate::validator_config::load_parts(destination_snapshot_base)?;
    let address = crate::parse_socket_address(address)?;
    let client = crate::quic_client(server_certificate)?;
    let result = async {
        let peer = client
            .connect(address)
            .await
            .map_err(|error| format!("failed to connect handoff provider: {error:?}"))?;
        let proof =
            second::client_validator_set_transition_proof(&peer, trusted.validator_set.version())
                .await;
        peer.close();
        let proof = proof
            .map_err(|error| format!("handoff proof query failed: {error:?}"))?
            .ok_or_else(|| {
                "provider has no transition proof for the trusted committee".to_owned()
            })?;
        // Public proof sessions stay read-only. Private body authorization binds
        // the fresh connection, the next committee identity and the exact root.
        // The fetcher is the single proof/identity verifier and checks both
        // before any private request. Empty genesis needs no second connection.
        let peer = if proof.source().task_handoff_digest().is_some() {
            client
                .connect(address)
                .await
                .map_err(|error| format!("failed to connect handoff body provider: {error:?}"))?
        } else {
            peer
        };
        let body = second::client_fetch_validator_handoff(
            &peer,
            material.validator_id(),
            material.identity_key(),
            &proof,
            &trusted,
        )
        .await;
        peer.close();
        let body = body.map_err(|error| format!("handoff baseline fetch failed: {error:?}"))?;
        Ok::<_, String>((proof, body))
    }
    .await;
    client.wait_idle().await;
    let (proof, body) = result?;
    destination
        .install_validator_handoff_baseline(&proof, &body, &trusted, &authorizers)
        .map_err(|error| format!("failed to install handoff baseline: {error:?}"))?;
    println!(
        "HANDOFF-INSTALLED validator={} validator_set={} safety=locked snapshot={}",
        material.validator_id().value(),
        proof.source().next_validator_set().version(),
        destination_snapshot_base
    );
    Ok(())
}
