use std::collections::BTreeMap;
use std::io::{self, Write as _};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use ed25519_dalek::SigningKey;
use second::{AccountAddress, AuthorizerSet, LegalTask, LegalTaskStatus, PaymentAddress};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::client::WalletNode;
use super::storage::{self, WalletSession};
use super::{AccountMaterial, StoredRequest, WalletData, WalletNetwork};
use crate::local_file;

pub(super) const HELP: &str = "second wallet init <directory> <trust-snapshot> <network-json>\nsecond wallet restore <backup-file> <new-directory>\nsecond wallet <command> <directory> [arguments] [--password-file <private-file>]\ncommands: open, accounts, account create/import/export/use, register, address, address create/retire/finalize, balance, assets, send, status, retry, history, requests, export-request, authorize, contacts, contact add/remove, backup, password-change, authorizer import/remove, sync";

pub(super) fn init(
    directory: &Path,
    trust: &Path,
    network_file: &Path,
    password: &str,
) -> Result<(), String> {
    let mut network: WalletNetwork = serde_json::from_slice(&local_file::read_bounded(
        network_file,
        256 * 1024,
        "wallet network configuration",
    )?)
    .map_err(|error| format!("invalid wallet network configuration: {error}"))?;
    let authorizer_seed = if let Some(path) = network.authorizer_key_file.take() {
        let path = Path::new(&path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            network_file.parent().unwrap_or(Path::new(".")).join(path)
        };
        Some(read_seed(&path)?)
    } else {
        None
    };
    let data = WalletData {
        version: 1,
        network,
        authorizer_seed,
        accounts: BTreeMap::from([("default".to_owned(), new_account()?)]),
        active: "default".to_owned(),
        contacts: BTreeMap::new(),
        requests: BTreeMap::new(),
    };
    data.validate()?;
    // Refuse an existing target before creating either the vault or trust snapshot.
    std::fs::create_dir(directory)
        .map_err(|error| format!("wallet init requires a new directory: {error}"))?;
    crate::cli_public::public_init(
        &directory.join("node").display().to_string(),
        &trust.display().to_string(),
    )?;
    let session = WalletSession::create(directory, data, password)?;
    println!(
        "WALLET created={} account={} registration=not_registered",
        directory.display(),
        account_address(&session.data)?
    );
    Ok(())
}

fn new_account() -> Result<AccountMaterial, String> {
    let key = local_file::random_signing_key()?;
    let mut address = [0; 32];
    getrandom::fill(&mut address).map_err(|error| error.to_string())?;
    Ok(AccountMaterial {
        seed: key.to_bytes(),
        initial_payment: PaymentAddress::from_bytes(address).to_string(),
    })
}

fn read_seed(path: &Path) -> Result<[u8; 32], String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Seed {
        private_key_base64: String,
    }
    local_file::validate_private_file_permissions(path)?;
    let bytes = Zeroizing::new(local_file::read_bounded(path, 4096, "private key")?);
    let mut seed: Seed = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let result =
        local_file::decode_standard_base64_32(&seed.private_key_base64).map_err(str::to_owned);
    use zeroize::Zeroize as _;
    seed.private_key_base64.zeroize();
    result
}

fn account_address(data: &WalletData) -> Result<AccountAddress, String> {
    Ok(AccountAddress::from_bytes(
        data.account_key()?.verifying_key().to_bytes(),
    ))
}

fn account_task(request: &StoredRequest) -> Result<LegalTask, String> {
    let mut signed = request.unsigned.clone();
    signed
        .as_object_mut()
        .ok_or("request is not an object")?
        .insert(
            "account_signatures".to_owned(),
            json!([{"account":request.account,"signature":request.account_signature_base64}]),
        );
    second::parse_transaction_request_json(
        &serde_json::to_vec(&signed).map_err(|error| error.to_string())?,
        [0; 32],
    )
    .map_err(|error| format!("invalid wallet request: {error:?}"))
}

pub(super) fn signed_task(request: &StoredRequest) -> Result<Option<LegalTask>, String> {
    let account_task = account_task(request)?;
    let needs_authorizer = account_task.payload().operations().iter().any(|operation| {
        matches!(
            operation,
            second::Operation::Issue { .. }
                | second::Operation::Destroy { .. }
                | second::Operation::LeakRepair { .. }
        )
    });
    match (&request.signed, request.authorizer) {
        (Some(bytes), Some(key)) => {
            let authority = second::parse_transaction_request_json(bytes, key)
                .map_err(|error| format!("invalid authorized request: {error:?}"))?;
            if authority.payload() != account_task.payload() {
                return Err("Authorizer changed wallet request".to_owned());
            }
            Ok(Some(LegalTask::from_signed_parts(
                authority.payload().clone(),
                authority.network_id(),
                authority.authorizer_public_key(),
                authority.signature_bytes(),
                account_task.account_signatures().to_vec(),
            )))
        }
        (None, None) if !needs_authorizer => Ok(Some(account_task)),
        (None, None) => Ok(None),
        _ => Err("incomplete authorized request".to_owned()),
    }
}

pub(super) fn validate_request(
    id: &str,
    request: &StoredRequest,
    authorizers: &AuthorizerSet,
) -> Result<(), String> {
    let task = account_task(request)?;
    if task.payload().task_id().as_str() != id {
        return Err("wallet task id mismatch".to_owned());
    }
    let account = AccountAddress::parse(&request.account).map_err(|_| "invalid account")?;
    if task.account_signatures().len() != 1 || !task.has_account_signature(account) {
        return Err("wrong wallet account signature".to_owned());
    }
    if task.network_id() != authorizers.network_id() {
        return Err("wallet request belongs to a different network".to_owned());
    }
    task.verify_account_signatures()
        .map_err(|error| format!("invalid account intent signature: {error:?}"))?;
    if let Some(signed) = signed_task(request)? {
        signed
            .verify(authorizers)
            .map_err(|error| format!("invalid wallet authorization: {error:?}"))?;
    }
    Ok(())
}

fn save_request(session: &mut WalletSession, operations: Value) -> Result<String, String> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    let id = crate::hex_digest(&bytes);
    let unsigned = json!({ "request_id": id, "version": 1, "expires_at": null, "network_id_base64": session.data.network.network_id_base64, "operations": operations });
    let encoded = serde_json::to_vec(&unsigned).map_err(|error| error.to_string())?;
    let signed_intent =
        second::sign_account_transaction_request_json(&encoded, &session.data.account_key()?)
            .map_err(|error| format!("invalid wallet intent: {error:?}"))?;
    let intent: Value =
        serde_json::from_slice(&signed_intent).map_err(|error| error.to_string())?;
    let requires_authorizer = unsigned["operations"].as_array().is_some_and(|operations| {
        operations.iter().any(|op| {
            matches!(
                op["type"].as_str(),
                Some("issue" | "destroy" | "leak_repair")
            )
        })
    });
    let (signed, authorizer) = match session.data.authorizer_seed.filter(|_| requires_authorizer) {
        Some(seed) => {
            let key = SigningKey::from_bytes(&seed);
            (
                Some(
                    second::sign_transaction_request_json(&encoded, &key)
                        .map_err(|error| format!("wallet signing: {error:?}"))?,
                ),
                Some(key.verifying_key().to_bytes()),
            )
        }
        None => (None, None),
    };
    let request = StoredRequest {
        account: account_address(&session.data)?.to_string(),
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_secs(),
        unsigned,
        account_signature_base64: intent["account_signatures"][0]["signature"]
            .as_str()
            .ok_or("missing account signature")?
            .to_owned(),
        signed,
        authorizer,
    };
    validate_request(&id, &request, &session.data.authorizers()?)?;
    session.data.requests.insert(id.clone(), request);
    session.save()?; // Never send before the exact original intent is durable.
    println!("REQUEST task={id} saved=true");
    Ok(id)
}

fn show_status(id: &str, status: LegalTaskStatus) -> Result<(), String> {
    println!(
        "TASK task={id} state={}",
        format!("{status:?}").to_lowercase()
    );
    if status == LegalTaskStatus::Cancelled {
        Err(format!("task {id} was cancelled"))
    } else {
        Ok(())
    }
}

async fn transmit(session: &WalletSession, node: &WalletNode, id: &str) -> Result<(), String> {
    let request = session
        .data
        .requests
        .get(id)
        .ok_or("unknown wallet request")?;
    if signed_task(request)?.is_none() {
        println!("TASK task={id} state=awaiting_authorizer");
        return Ok(());
    }
    show_status(id, node.submit(&session.data, request, true).await?)
}

pub(super) async fn execute(session: &mut WalletSession, args: &[String]) -> Result<(), String> {
    let network_command = args.first().is_some_and(|command| {
        matches!(
            command.as_str(),
            "register" | "balance" | "assets" | "send" | "status" | "retry" | "history" | "sync"
        ) || (command == "address" && args.len() > 1)
    });
    let node = if network_command {
        Some(WalletNode::start(session)?)
    } else {
        None
    };
    execute_with_node(session, args, node.as_ref()).await
}

async fn execute_with_node(
    session: &mut WalletSession,
    args: &[String],
    node: Option<&WalletNode>,
) -> Result<(), String> {
    let network = || node.ok_or_else(|| "wallet node is not running".to_owned());
    match args {
        [command] if command == "help" => {
            println!("{HELP}");
            Ok(())
        }
        [command] if command == "accounts" => {
            for (label, account) in &session.data.accounts {
                println!(
                    "ACCOUNT label={label} address={} active={}",
                    AccountAddress::from_bytes(
                        SigningKey::from_bytes(&account.seed)
                            .verifying_key()
                            .to_bytes()
                    ),
                    label == &session.data.active
                );
            }
            Ok(())
        }
        [command, action, label] if command == "account" && action == "create" => {
            if session.data.accounts.contains_key(label) {
                return Err("account label already exists".to_owned());
            }
            session.data.accounts.insert(label.clone(), new_account()?);
            session.data.active = label.clone();
            session.save()?;
            println!(
                "ACCOUNT label={label} address={} registration=not_registered",
                account_address(&session.data)?
            );
            Ok(())
        }
        [command, action, label, file] if command == "account" && action == "import" => {
            if session.data.accounts.contains_key(label) {
                return Err("account label already exists".to_owned());
            }
            let seed = read_seed(Path::new(file))?;
            if session
                .data
                .accounts
                .values()
                .any(|account| account.seed == seed)
            {
                return Err("account key already exists in wallet".to_owned());
            }
            let mut account = new_account()?;
            account.seed = seed;
            session.data.accounts.insert(label.clone(), account);
            session.data.active = label.clone();
            session.save()?;
            println!(
                "ACCOUNT imported={} address={}",
                label,
                account_address(&session.data)?
            );
            Ok(())
        }
        [command, action, label, file] if command == "account" && action == "export" => {
            let account = session
                .data
                .accounts
                .get(label)
                .ok_or("unknown account label")?;
            let bytes = Zeroizing::new(serde_json::to_vec(&json!({"private_key_base64":base64::engine::general_purpose::STANDARD.encode(account.seed)})).map_err(|error| error.to_string())?);
            local_file::write_new_private(Path::new(file), &bytes, "plaintext account key export")?;
            println!(
                "KEY exported={file} protection=plaintext_private_file; use backup for encrypted recovery"
            );
            Ok(())
        }
        [command, action, label] if command == "account" && action == "use" => {
            if !session.data.accounts.contains_key(label) {
                return Err("unknown account label".to_owned());
            }
            session.data.active = label.clone();
            session.save()?;
            println!("ACCOUNT active={label}");
            Ok(())
        }
        [command] if command == "address" => {
            println!(
                "ADDRESS account={} payment={} registration=check_with_address_list",
                account_address(&session.data)?,
                session.data.accounts[&session.data.active].initial_payment
            );
            Ok(())
        }
        [command] if command == "register" => {
            let node = network()?;
            let view = node.query(&session.data, 2).await?;
            let account = account_address(&session.data)?.to_string();
            let payment = session.data.accounts[&session.data.active]
                .initial_payment
                .clone();
            if view
                .addresses
                .iter()
                .any(|address| address.address == payment)
            {
                println!("REGISTERED account={account} payment={payment}");
                return Ok(());
            }
            let mut operations = Vec::new();
            if !view.exists {
                operations.push(json!({"type":"register_account","account":account}));
            }
            operations.push(
                json!({"type":"register_payment_address","address":payment,"account":account}),
            );
            let operations = json!(operations);
            let existing = session
                .data
                .requests
                .iter()
                .find(|(_, request)| {
                    request.account == account && request.unsigned["operations"] == operations
                })
                .map(|(id, _)| id.clone());
            let id = match existing {
                Some(id) => id,
                None => save_request(session, operations)?,
            };
            transmit(session, node, &id).await
        }
        [command, action] if command == "address" && action == "list" => {
            let view = network()?.query(&session.data, 2).await?;
            print_view(&view)?;
            Ok(())
        }
        [command, action] if command == "address" && action == "create" => {
            let node = network()?;
            if !node.query(&session.data, 1).await?.exists {
                return Err("register the account first".to_owned());
            }
            let mut bytes = [0; 32];
            getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
            let payment = PaymentAddress::from_bytes(bytes).to_string();
            let id = save_request(
                session,
                json!([{"type":"register_payment_address","address":payment,"account":account_address(&session.data)?.to_string()}]),
            )?;
            println!("ADDRESS proposed={payment} task={id}");
            transmit(session, node, &id).await
        }
        [command, action, payment]
            if command == "address" && matches!(action.as_str(), "retire" | "finalize") =>
        {
            let node = network()?;
            let view = node.query(&session.data, 2).await?;
            let own = view
                .addresses
                .iter()
                .find(|address| &address.address == payment)
                .ok_or("payment address is not owned by active account")?;
            let expected = if action == "retire" {
                "active"
            } else {
                "retiring"
            };
            if own.status != expected {
                return Err(format!("address must be {expected}"));
            }
            let op = if action == "retire" {
                "retire_payment_address"
            } else {
                "finalize_payment_address_retirement"
            };
            let id = save_request(session, json!([{"type":op,"address":payment}]))?;
            transmit(session, node, &id).await
        }
        [command] if matches!(command.as_str(), "balance" | "assets" | "history") => {
            let kind = match command.as_str() {
                "balance" => 1,
                "assets" => 4,
                _ => 3,
            };
            let view = network()?.query(&session.data, kind).await?;
            print_view(&view)?;
            Ok(())
        }
        [command, destination, amount, rest @ ..] if command == "send" => {
            if !rest.is_empty() && rest != ["--yes"] {
                return Err("send accepts only optional --yes".to_owned());
            }
            let destination = session
                .data
                .contacts
                .get(destination)
                .unwrap_or(destination)
                .clone();
            PaymentAddress::parse(&destination)
                .map_err(|_| "invalid destination payment address")?;
            let amount = amount
                .parse::<u64>()
                .map_err(|_| "amount must be a positive whole Secoin count")?;
            if amount == 0 {
                return Err("amount must be positive".to_owned());
            }
            let node = network()?;
            let view = node.query(&session.data, 2).await?;
            if !view.exists {
                return Err("account has not been registered".to_owned());
            }
            if view.balance < amount {
                return Err(format!(
                    "insufficient balance: requested={amount} balance={}",
                    view.balance
                ));
            }
            let initial = &session.data.accounts[&session.data.active].initial_payment;
            let source = view
                .addresses
                .iter()
                .find(|address| &address.address == initial && address.status == "active")
                .or_else(|| {
                    view.addresses
                        .iter()
                        .find(|address| address.status == "active")
                })
                .ok_or("no active source payment address")?
                .address
                .clone();
            println!(
                "PAYMENT account={} source={source} destination={destination} amount={amount}",
                view.account
            );
            if rest.is_empty() {
                confirm_payment().await?;
            }
            let id = save_request(
                session,
                json!([{"type":"transfer","source":source,"destination":destination,"amount":amount}]),
            )?;
            transmit(session, node, &id).await
        }
        [command, id] if command == "status" || command == "retry" => {
            let node = network()?;
            let request = session
                .data
                .requests
                .get(id)
                .ok_or("unknown wallet request")?;
            if signed_task(request)?.is_none() {
                println!("TASK task={id} state=awaiting_authorizer");
                return Ok(());
            }
            if command == "retry" {
                transmit(session, node, id).await
            } else {
                show_status(id, node.status(&session.data, request).await?)
            }
        }
        [command] if command == "requests" => {
            for (id, request) in &session.data.requests {
                println!(
                    "REQUEST task={id} account={} created_at={} state={} operations={}",
                    request.account,
                    request.created_at,
                    if signed_task(request)?.is_some() {
                        "saved_signed_query_status"
                    } else {
                        "awaiting_authorizer"
                    },
                    request.unsigned["operations"]
                );
            }
            Ok(())
        }
        [command, id, file] if command == "export-request" => {
            let request = session
                .data
                .requests
                .get(id)
                .ok_or("unknown wallet request")?;
            let bytes = serde_json::to_vec_pretty(&json!({"account":request.account,"account_signature_base64":request.account_signature_base64,"unsigned_transaction":request.unsigned})).map_err(|error| error.to_string())?;
            local_file::write_new_private(Path::new(file), &bytes, "wallet signed intent")?;
            println!("INTENT exported={file} task={id}");
            Ok(())
        }
        [command, id, file, public_key] if command == "authorize" => {
            let authorizer =
                local_file::decode_standard_base64_32(public_key).map_err(str::to_owned)?;
            let signed = local_file::read_bounded(
                Path::new(file),
                crate::cli_legal_task::MAX_TRANSACTION_REQUEST_JSON_SIZE,
                "authorized wallet transaction",
            )?;
            let original = session
                .data
                .requests
                .get(id)
                .ok_or("unknown wallet request")?;
            let candidate = StoredRequest {
                account: original.account.clone(),
                created_at: original.created_at,
                unsigned: original.unsigned.clone(),
                account_signature_base64: original.account_signature_base64.clone(),
                signed: Some(signed),
                authorizer: Some(authorizer),
            };
            validate_request(id, &candidate, &session.data.authorizers()?)?;
            session.data.requests.insert(id.clone(), candidate);
            session.save()?;
            println!("TASK task={id} state=signed_not_submitted");
            Ok(())
        }
        [command] if command == "contacts" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&session.data.contacts)
                    .map_err(|error| error.to_string())?
            );
            Ok(())
        }
        [command, action, label, payment] if command == "contact" && action == "add" => {
            PaymentAddress::parse(payment).map_err(|_| "invalid contact address")?;
            if label.is_empty() || label.len() > 128 || session.data.contacts.contains_key(label) {
                return Err("invalid or existing contact label".to_owned());
            }
            session.data.contacts.insert(label.clone(), payment.clone());
            session.save()?;
            Ok(())
        }
        [command, action, label] if command == "contact" && action == "remove" => {
            session
                .data
                .contacts
                .remove(label)
                .ok_or("unknown contact")?;
            session.save()
        }
        [command, file] if command == "backup" => {
            storage::backup(session, Path::new(file))?;
            println!("BACKUP encrypted={file}");
            Ok(())
        }
        [command, rest @ ..] if command == "password-change" && rest.len() <= 1 => {
            let password = storage::password(rest.first().map(String::as_str), true)?;
            session.cipher = storage::VaultCipher::create(&password)?;
            session.save()?;
            println!("PASSWORD changed=true");
            Ok(())
        }
        [command, action, file] if command == "authorizer" && action == "import" => {
            let seed = read_seed(Path::new(file))?;
            if !session
                .data
                .authorizers()?
                .contains(SigningKey::from_bytes(&seed).verifying_key().to_bytes())
            {
                return Err("Authorizer key is not trusted by this wallet network".to_owned());
            }
            session.data.authorizer_seed = Some(seed);
            session.save()?;
            println!("AUTHORIZER imported=true");
            Ok(())
        }
        [command, action] if command == "authorizer" && action == "remove" => {
            session.data.authorizer_seed = None;
            session.save()?;
            println!("AUTHORIZER removed=true");
            Ok(())
        }
        [command] if command == "sync" => {
            network()?.sync(&session.data).await?;
            println!("SYNC public_state=certified");
            Ok(())
        }
        _ => Err(HELP.to_owned()),
    }
}

fn print_view(view: &second::AccountView) -> Result<(), String> {
    println!(
        "{}",
        serde_json::to_string_pretty(view).map_err(|error| error.to_string())?
    );
    println!(
        "SOURCE authenticated_node_snapshot=true independent_account_finality_proof=false network_latest_guaranteed=false linearizable_read=false"
    );
    Ok(())
}

async fn confirm_payment() -> Result<(), String> {
    tokio::task::spawn_blocking(|| {
        print!("Confirm payment [yes/no]: ");
        io::stdout().flush().map_err(|error| error.to_string())?;
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        if line.trim() == "yes" {
            Ok(())
        } else {
            Err("payment not submitted".to_owned())
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

pub(super) async fn open(session: &mut WalletSession) -> Result<(), String> {
    let node = WalletNode::start(session)?;
    println!(
        "WALLET network={} account={} node={} listen={}",
        session.data.network.name,
        account_address(&session.data)?,
        node.runtime.node_id(),
        node.runtime
            .local_addr()
            .map_err(|error| format!("{error:?}"))?
    );
    println!("Type help for commands; exit closes this wallet node.");
    loop {
        let line = tokio::task::spawn_blocking(|| {
            print!("wallet> ");
            io::stdout().flush().map_err(|error| error.to_string())?;
            let mut line = String::new();
            if io::stdin()
                .read_line(&mut line)
                .map_err(|error| error.to_string())?
                == 0
            {
                return Ok(None);
            }
            Ok::<_, String>(Some(line))
        })
        .await
        .map_err(|error| error.to_string())??;
        let Some(line) = line else {
            break;
        };
        let args = match parse_line(&line) {
            Ok(args) => args,
            Err(error) => {
                eprintln!("{error}");
                continue;
            }
        };
        if args.is_empty() {
            continue;
        }
        if args == ["exit"] {
            break;
        }
        if let Err(error) = execute_with_node(session, &args, Some(&node)).await {
            eprintln!("{error}");
        }
    }
    Ok(())
}

fn parse_line(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    for character in line.chars() {
        if let Some(delimiter) = quote {
            if character == delimiter {
                quote = None;
            } else {
                word.push(character);
            }
        } else if character == '\'' || character == '"' {
            quote = Some(character);
            started = true;
        } else if character.is_whitespace() {
            if started {
                words.push(std::mem::take(&mut word));
                started = false;
            }
        } else {
            word.push(character);
            started = true;
        }
    }
    if quote.is_some() {
        return Err("unclosed command quote".to_owned());
    }
    if started {
        words.push(word);
    }
    Ok(words)
}
