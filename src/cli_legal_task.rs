use std::path::Path;

use second::{
    LegalTask, LegalTaskStatus, LegalTaskStatusRejection, LegalTaskSubmissionOutcome,
    LegalTaskSubmissionRejection, NetworkError, client_legal_task_status, client_submit_legal_task,
    parse_transaction_request_json,
};

use crate::{local_file, parse_socket_address, quic_client};

const MAX_TRANSACTION_REQUEST_JSON_SIZE: usize = 16 * 1024 * 1024;

pub(crate) async fn submit(
    address: &str,
    transaction_file: &str,
    authorizer_public_key: &str,
    server_certificate: &str,
) -> Result<(), String> {
    let task = load_transaction_task(transaction_file, authorizer_public_key)?;

    let client = quic_client(server_certificate)?;
    let peer = client
        .connect(parse_socket_address(address)?)
        .await
        .map_err(|error| format!("failed to connect QUIC peer {address}: {error:?}"))?;
    let result = client_submit_legal_task(&peer, &task).await;
    peer.close();
    client.wait_idle().await;

    let submitted = result.map_err(submission_error)?;
    let state = match submitted.outcome {
        LegalTaskSubmissionOutcome::Prepared => "prepared",
        LegalTaskSubmissionOutcome::AlreadyPending => "pending",
        LegalTaskSubmissionOutcome::AlreadySucceeded => "succeeded",
    };
    println!("ACCEPTED task={} state={state}", submitted.task_id);
    Ok(())
}

pub(crate) async fn task_status(
    address: &str,
    transaction_file: &str,
    authorizer_public_key: &str,
    server_certificate: &str,
) -> Result<(), String> {
    let task = load_transaction_task(transaction_file, authorizer_public_key)?;
    let client = quic_client(server_certificate)?;
    let peer = client
        .connect(parse_socket_address(address)?)
        .await
        .map_err(|error| format!("failed to connect QUIC peer {address}: {error:?}"))?;
    let result = client_legal_task_status(&peer, &task).await;
    peer.close();
    client.wait_idle().await;

    let remote = result.map_err(task_status_error)?;
    let state = match remote.status {
        LegalTaskStatus::Unknown => "unknown",
        LegalTaskStatus::Bound => "bound",
        LegalTaskStatus::Prepared => "prepared",
        LegalTaskStatus::Voting => "voting",
        LegalTaskStatus::Finalized => "finalized",
        LegalTaskStatus::Succeeded => "succeeded",
    };
    println!("TASK task={} state={state}", remote.task_id);
    Ok(())
}

fn task_status_error(error: NetworkError) -> String {
    match error {
        NetworkError::LegalTaskStatusRejected(LegalTaskStatusRejection::Unavailable) => {
            "target node does not provide private LegalTask status".to_owned()
        }
        NetworkError::LegalTaskStatusRejected(LegalTaskStatusRejection::Rejected) => {
            "LegalTask status query was rejected".to_owned()
        }
        other => format!("LegalTask status query failed: {other:?}"),
    }
}

fn load_transaction_task(
    transaction_file: &str,
    authorizer_public_key: &str,
) -> Result<LegalTask, String> {
    let request = local_file::read_bounded(
        Path::new(transaction_file),
        MAX_TRANSACTION_REQUEST_JSON_SIZE,
        "transaction request",
    )?;
    let authorizer_public_key = local_file::decode_standard_base64_32(authorizer_public_key)
        .map_err(|error| format!("invalid authorizer public key: {error}"))?;
    parse_transaction_request_json(&request, authorizer_public_key)
        .map_err(|error| format!("invalid transaction request: {error:?}"))
}

fn submission_error(error: NetworkError) -> String {
    match error {
        NetworkError::LegalTaskSubmissionRejected(LegalTaskSubmissionRejection::Unavailable) => {
            "target node does not provide LegalTask submission".to_owned()
        }
        NetworkError::LegalTaskSubmissionRejected(LegalTaskSubmissionRejection::Busy) => {
            "target node is temporarily at its LegalTask submission capacity".to_owned()
        }
        NetworkError::LegalTaskSubmissionRejected(LegalTaskSubmissionRejection::Rejected) => {
            "LegalTask submission was rejected".to_owned()
        }
        other => format!("LegalTask submission failed: {other:?}"),
    }
}
