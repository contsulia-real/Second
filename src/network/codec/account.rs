use super::super::{AccountView, NetworkError, NetworkMessage};
use super::require_message_length;

pub(super) fn encode(message: &NetworkMessage) -> Result<Vec<u8>, NetworkError> {
    let mut out = Vec::new();
    match message {
        NetworkMessage::AccountQuery {
            account,
            kind,
            cursor,
            generation,
            nonce,
            signature,
        } => {
            super::super::account_query::validate_request(*kind, *cursor, *generation)?;
            out.push(44);
            out.extend_from_slice(account);
            out.push(*kind);
            out.extend_from_slice(&cursor.to_be_bytes());
            out.push(u8::from(generation.is_some()));
            out.extend_from_slice(&generation.unwrap_or(0).to_be_bytes());
            out.extend_from_slice(nonce);
            out.extend_from_slice(signature);
        }
        NetworkMessage::AccountQueryResult { view } => {
            out.push(45);
            out.extend_from_slice(
                &serde_json::to_vec(view).map_err(|_| NetworkError::InvalidAccountQuery)?,
            );
        }
        NetworkMessage::AccountQueryDenied { reason } => {
            out.push(46);
            out.push(match reason.as_str() {
                "unauthorized" => 1,
                "unavailable" => 2,
                "invalid_query" => 3,
                "budget_exceeded" => 4,
                "busy" => 5,
                "session_expired" => 6,
                _ => return Err(NetworkError::InvalidAccountQuery),
            });
        }
        _ => return Err(NetworkError::UnexpectedMessage),
    }
    Ok(out)
}

pub(super) fn decode(kind: u8, payload: &[u8]) -> Result<NetworkMessage, NetworkError> {
    match kind {
        44 => {
            require_message_length(kind, payload, 147)?;
            let generation = u64::from_be_bytes(payload[43..51].try_into().unwrap());
            let generation = match payload[42] {
                0 if generation == 0 => None,
                1 => Some(generation),
                _ => return Err(NetworkError::InvalidAccountQuery),
            };
            super::super::account_query::validate_request(
                payload[33],
                u64::from_be_bytes(payload[34..42].try_into().unwrap()),
                generation,
            )?;
            Ok(NetworkMessage::AccountQuery {
                account: payload[1..33].try_into().unwrap(),
                kind: payload[33],
                cursor: u64::from_be_bytes(payload[34..42].try_into().unwrap()),
                generation,
                nonce: payload[51..83].try_into().unwrap(),
                signature: payload[83..147].try_into().unwrap(),
            })
        }
        45 => Ok(NetworkMessage::AccountQueryResult {
            view: serde_json::from_slice::<AccountView>(&payload[1..])
                .map_err(|_| NetworkError::InvalidAccountQuery)?,
        }),
        46 => {
            require_message_length(kind, payload, 2)?;
            Ok(NetworkMessage::AccountQueryDenied {
                reason: match payload[1] {
                    1 => "unauthorized",
                    2 => "unavailable",
                    3 => "invalid_query",
                    4 => "budget_exceeded",
                    5 => "busy",
                    6 => "session_expired",
                    _ => return Err(NetworkError::InvalidAccountQuery),
                }
                .to_owned(),
            })
        }
        _ => Err(NetworkError::UnknownMessageType(kind)),
    }
}
