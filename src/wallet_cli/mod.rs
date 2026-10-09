mod client;
mod commands;
mod storage;

use std::collections::BTreeMap;
use std::path::Path;

use base64::Engine as _;
use ed25519_dalek::SigningKey;
use second::{AuthorizerSet, CURRENT_PROTOCOL_VERSION, PaymentAddress};
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WalletNetwork {
    pub name: String,
    pub network_id_base64: String,
    pub endpoints: Vec<Endpoint>,
    pub authorizer_public_keys_base64: Vec<String>,
    pub authorizer_key_file: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Endpoint {
    pub address: String,
    pub certificate_base64: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AccountMaterial {
    pub seed: [u8; 32],
    pub initial_payment: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredRequest {
    pub account: String,
    pub created_at: u64,
    pub unsigned: serde_json::Value,
    pub account_signature_base64: String,
    pub signed: Option<Vec<u8>>,
    pub authorizer: Option<[u8; 32]>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WalletData {
    version: u32,
    network: WalletNetwork,
    authorizer_seed: Option<[u8; 32]>,
    accounts: BTreeMap<String, AccountMaterial>,
    active: String,
    contacts: BTreeMap<String, String>,
    requests: BTreeMap<String, StoredRequest>,
}

impl Drop for WalletData {
    fn drop(&mut self) {
        for account in self.accounts.values_mut() {
            account.seed.zeroize();
        }
        if let Some(seed) = &mut self.authorizer_seed {
            seed.zeroize();
        }
    }
}

impl WalletData {
    fn account_key(&self) -> Result<SigningKey, String> {
        self.accounts
            .get(&self.active)
            .map(|account| SigningKey::from_bytes(&account.seed))
            .ok_or_else(|| "active account missing".to_owned())
    }
    fn authorizers(&self) -> Result<AuthorizerSet, String> {
        let keys = self
            .network
            .authorizer_public_keys_base64
            .iter()
            .map(|key| crate::local_file::decode_standard_base64_32(key).map_err(str::to_owned))
            .collect::<Result<Vec<_>, _>>()?;
        let network_id =
            crate::local_file::decode_standard_base64_32(&self.network.network_id_base64)
                .map_err(str::to_owned)?;
        AuthorizerSet::new_for_network(CURRENT_PROTOCOL_VERSION, network_id, keys)
            .map_err(|error| format!("invalid wallet authorizers: {error:?}"))
    }
    fn validate(&self) -> Result<(), String> {
        if self.version != 1
            || self.accounts.is_empty()
            || self.accounts.len() > 256
            || !self.accounts.contains_key(&self.active)
            || self.network.endpoints.is_empty()
            || self.network.endpoints.len() > 16
            || self.network.name.is_empty()
        {
            return Err("invalid wallet configuration".to_owned());
        }
        for endpoint in &self.network.endpoints {
            crate::parse_socket_address(&endpoint.address)?;
            let cert = base64::engine::general_purpose::STANDARD
                .decode(&endpoint.certificate_base64)
                .map_err(|_| "invalid endpoint certificate")?;
            if cert.is_empty() || cert.len() > second::MAX_PEER_CERTIFICATE_SIZE {
                return Err("invalid endpoint certificate size".to_owned());
            }
        }
        let authorizers = self.authorizers()?;
        if let Some(seed) = self.authorizer_seed
            && !authorizers.contains(SigningKey::from_bytes(&seed).verifying_key().to_bytes())
        {
            return Err(
                "local Authorizer key is not trusted by wallet network configuration".to_owned(),
            );
        }
        for (label, account) in &self.accounts {
            if label.is_empty() || label.len() > 128 {
                return Err("invalid account label".to_owned());
            }
            PaymentAddress::parse(&account.initial_payment)
                .map_err(|_| "invalid wallet payment address")?;
        }
        for payment in self.contacts.values() {
            PaymentAddress::parse(payment).map_err(|_| "invalid contact payment address")?;
        }
        for (id, request) in &self.requests {
            commands::validate_request(id, request, &authorizers)?;
        }
        Ok(())
    }
}

pub(crate) async fn run(args: &[String]) -> Result<(), String> {
    let mut args = args.to_vec();
    let mut password_file = None;
    if let Some(index) = args.iter().position(|arg| arg == "--password-file") {
        if index + 1 >= args.len() {
            return Err("--password-file requires a file".to_owned());
        }
        password_file = Some(args.remove(index + 1));
        args.remove(index);
    }
    match args.as_slice() {
        [command, directory, trust, network] if command == "init" => {
            let password = storage::password(password_file.as_deref(), true)?;
            commands::init(
                Path::new(directory),
                Path::new(trust),
                Path::new(network),
                &password,
            )
        }
        [command, backup, directory] if command == "restore" => {
            let password = storage::password(password_file.as_deref(), false)?;
            storage::restore(Path::new(backup), Path::new(directory), &password)?;
            println!("WALLET restored={directory}");
            Ok(())
        }
        [command, directory, rest @ ..] => {
            let password = storage::password(password_file.as_deref(), false)?;
            let mut session = storage::WalletSession::load(Path::new(directory), &password)?;
            if command == "open" && rest.is_empty() {
                return commands::open(&mut session).await;
            }
            let mut command_args = vec![command.clone()];
            command_args.extend_from_slice(rest);
            commands::execute(&mut session, &command_args).await
        }
        _ => Err(commands::HELP.to_owned()),
    }
}
