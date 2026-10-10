//! Actual native wallet clients, each pinned exclusively to a validator on the other OS.
use super::*;
use second::AccountView;

pub(super) fn business_operations() -> (Vec<Operation>, Value) {
    let alice = account(7101);
    let bob = account(7102);
    let separator = account(7104);
    let source = payment(7101);
    let destination = payment(7102);
    let mut operations = vec![
        Operation::RegisterAccount { account: alice },
        Operation::RegisterAccount { account: bob },
        Operation::RegisterAccount { account: separator },
        Operation::RegisterPaymentAddress {
            address: source,
            account: alice,
        },
        Operation::RegisterPaymentAddress {
            address: destination,
            account: bob,
        },
    ];
    let mut request = vec![
        json!({"type":"register_account","account":alice.to_string()}),
        json!({"type":"register_account","account":bob.to_string()}),
        json!({"type":"register_account","account":separator.to_string()}),
        json!({"type":"register_payment_address","address":source.to_string(),"account":alice.to_string()}),
        json!({"type":"register_payment_address","address":destination.to_string(),"account":bob.to_string()}),
    ];
    // Keep three native wallet pages after assets switch from individual units to ranges.
    for index in 0..M0_ACCOUNT_ASSETS {
        let count = if index == 0 { 2 } else { 1 };
        operations.push(Operation::Issue {
            account: alice,
            count,
        });
        request.push(json!({"type":"issue","recipient":alice.to_string(),"amount":count}));
        if index + 1 < M0_ACCOUNT_ASSETS {
            operations.push(Operation::Issue {
                account: separator,
                count: 1,
            });
            request.push(json!({"type":"issue","recipient":separator.to_string(),"amount":1}));
        }
    }
    operations.push(Operation::Transfer {
        source,
        destination,
        amount: 1,
    });
    request.push(json!({"type":"transfer","source":source.to_string(),"destination":destination.to_string(),"amount":1}));
    (operations, Value::Array(request))
}

pub(super) fn check(
    worker: &mut LinuxWorker,
    root: &Path,
    distro: &str,
    bases: &[PathBuf],
    network: &Value,
) {
    let password = root.join("m0-password");
    let seed = root.join("m0-account.json");
    fs::write(&password, b"temporary-test-only-strong-password\n").unwrap();
    fs::write(
        &seed,
        serde_json::to_vec(&json!({
            "private_key_base64": STANDARD.encode(support::account_key(7101).to_bytes())
        }))
        .unwrap(),
    )
    .unwrap();
    let config = |index, name| {
        let path = root.join(name);
        let mut config = network.clone();
        config["endpoints"] = json!([network["endpoints"][index].clone()]);
        fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        path
    };
    let linux_target = config(2, "m0-linux-target.json");
    let windows_target = config(0, "m0-windows-target.json");
    let windows_wallet = root.join("m0-windows-wallet");
    let windows = query_wallet(
        cli,
        windows_wallet.to_str().unwrap(),
        bases[0].to_str().unwrap(),
        linux_target.to_str().unwrap(),
        password.to_str().unwrap(),
        seed.to_str().unwrap(),
    );
    let linux_root = worker.request(json!({"action":"info"}));
    let linux_root = linux_root.as_str().unwrap();
    for file in [&password, &seed, &windows_target] {
        worker.request(json!({
            "action":"copy", "source":wsl_path(distro,file),
            "destination":format!("{linux_root}/{}",file.file_name().unwrap().to_str().unwrap())
        }));
    }
    let linux = query_wallet(
        |args| worker.cli(args),
        &format!("{linux_root}/m0-linux-wallet"),
        bases[2].to_str().unwrap(),
        &format!("{linux_root}/m0-windows-target.json"),
        &format!("{linux_root}/m0-password"),
        &format!("{linux_root}/m0-account.json"),
    );
    // Local generation and nonce need not match across nodes or independent queries.
    assert_eq!(windows[1].currencies, linux[1].currencies);
    assert_eq!(windows[2].addresses, linux[2].addresses);
    assert_eq!(windows[3].transfers, linux[3].transfers);
    println!(
        "MIXED-PASS M0 native wallets query the other OS: balance, 257 asset ranges over three pages, addresses, history"
    );
}

fn query_wallet(
    mut invoke: impl FnMut(&[&str]) -> String,
    directory: &str,
    trust: &str,
    config: &str,
    password: &str,
    seed: &str,
) -> Vec<AccountView> {
    invoke(&[
        "wallet",
        "init",
        directory,
        trust,
        config,
        "--password-file",
        password,
    ]);
    invoke(&[
        "wallet",
        "account",
        directory,
        "import",
        "alice",
        seed,
        "--password-file",
        password,
    ]);
    let mut views = Vec::new();
    for command in [
        &["balance"][..],
        &["assets"][..],
        &["address", "list"][..],
        &["history"][..],
    ] {
        let mut args = vec!["wallet", command[0], directory];
        args.extend_from_slice(&command[1..]);
        args.extend_from_slice(&["--password-file", password]);
        let output = invoke(&args);
        let (json, source) = output.split_once("\nSOURCE ").unwrap();
        assert!(source.contains("independent_account_finality_proof=false"));
        assert!(source.contains("network_latest_guaranteed=false"));
        assert!(source.contains("linearizable_read=false"));
        let view: AccountView = serde_json::from_str(json).unwrap();
        assert_eq!(view.account, account(7101).to_string());
        assert!(view.exists);
        assert_eq!(view.balance, M0_ACCOUNT_ASSETS);
        assert_eq!(view.next, None);
        views.push(view);
    }
    assert!(views[0].currencies.is_empty());
    assert_eq!(views[1].total, M0_ACCOUNT_ASSETS);
    assert_eq!(views[1].currencies.len() as u64, M0_ACCOUNT_ASSETS);
    assert_eq!(
        views[1]
            .currencies
            .iter()
            .map(|range| range.len)
            .sum::<u64>(),
        M0_ACCOUNT_ASSETS
    );
    assert_eq!(views[2].addresses.len(), 1);
    assert_eq!(views[2].addresses[0].address, payment(7101).to_string());
    assert_eq!(views[3].transfers.len(), 1);
    assert!(views[3].transfers[0].outgoing);
    assert_eq!(views[3].transfers[0].amount, 1);
    views
}
