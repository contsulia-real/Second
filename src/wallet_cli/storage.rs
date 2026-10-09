use std::fs::File;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use ring::{aead, pbkdf2};
use second::DurableBlobStore;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use super::WalletData;
use crate::local_file;

const MAGIC: &[u8; 8] = b"S2WALV1\0";
const ITERATIONS: u32 = 600_000;
const MAX_FILE: usize = 64 * 1024 * 1024;

pub(super) struct VaultCipher {
    salt: [u8; 16],
    key: aead::LessSafeKey,
}

impl VaultCipher {
    fn derive(password: &str, salt: [u8; 16]) -> Result<Self, String> {
        if password.is_empty() {
            return Err("wallet password must not be empty".to_owned());
        }
        let mut material = Zeroizing::new([0; 32]);
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            NonZeroU32::new(ITERATIONS).unwrap(),
            &salt,
            password.as_bytes(),
            material.as_mut(),
        );
        let key = aead::UnboundKey::new(&aead::CHACHA20_POLY1305, material.as_ref())
            .map_err(|_| "wallet encryption key error")?;
        Ok(Self {
            salt,
            key: aead::LessSafeKey::new(key),
        })
    }

    pub(super) fn create(password: &str) -> Result<Self, String> {
        let mut salt = [0; 16];
        getrandom::fill(&mut salt).map_err(|error| error.to_string())?;
        Self::derive(password, salt)
    }

    pub(super) fn seal(&self, data: &WalletData) -> Result<Vec<u8>, String> {
        let mut nonce = [0; 12];
        getrandom::fill(&mut nonce).map_err(|error| error.to_string())?;
        let mut header = MAGIC.to_vec();
        header.extend_from_slice(&self.salt);
        header.extend_from_slice(&nonce);
        let mut plaintext =
            Zeroizing::new(serde_json::to_vec(data).map_err(|error| error.to_string())?);
        self.key
            .seal_in_place_append_tag(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(&header),
                &mut *plaintext,
            )
            .map_err(|_| "failed to encrypt wallet")?;
        header.extend_from_slice(&plaintext);
        Ok(header)
    }

    pub(super) fn open(password: &str, bytes: &[u8]) -> Result<(Self, WalletData), String> {
        if bytes.len() < 52 || bytes.len() > MAX_FILE || &bytes[..8] != MAGIC {
            return Err("invalid encrypted wallet".to_owned());
        }
        let cipher = Self::derive(password, bytes[8..24].try_into().unwrap())?;
        let mut plaintext = Zeroizing::new(bytes[36..].to_vec());
        let opened = cipher
            .key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(bytes[24..36].try_into().unwrap()),
                aead::Aad::from(&bytes[..36]),
                &mut plaintext,
            )
            .map_err(|_| "wrong wallet password or damaged wallet".to_owned())?;
        let data: WalletData =
            serde_json::from_slice(opened).map_err(|_| "invalid decrypted wallet".to_owned())?;
        data.validate()?;
        Ok((cipher, data))
    }
}

pub(super) struct WalletSession {
    pub directory: PathBuf,
    pub data: WalletData,
    pub cipher: VaultCipher,
    generation: u64,
    _lock: File,
}

impl WalletSession {
    pub fn create(directory: &Path, data: WalletData, password: &str) -> Result<Self, String> {
        data.validate()?;
        let lock = local_file::lock_node_directory(&directory.join("wallet"))?;
        let cipher = VaultCipher::create(password)?;
        let bytes = cipher.seal(&data)?;
        let generation = DurableBlobStore::new(directory.join("wallet"))
            .save(None, &bytes)
            .map_err(|error| format!("cannot create wallet: {error:?}"))?;
        Ok(Self {
            directory: directory.to_path_buf(),
            data,
            cipher,
            generation,
            _lock: lock,
        })
    }

    pub fn load(directory: &Path, password: &str) -> Result<Self, String> {
        let lock = local_file::lock_node_directory(&directory.join("wallet"))?;
        let (generation, bytes) = DurableBlobStore::new(directory.join("wallet"))
            .load()
            .map_err(|error| format!("cannot load wallet: {error:?}"))?
            .ok_or("wallet does not exist")?;
        let (cipher, data) = VaultCipher::open(password, &bytes)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            data,
            cipher,
            generation,
            _lock: lock,
        })
    }

    pub fn save(&mut self) -> Result<(), String> {
        self.data.validate()?;
        let bytes = self.cipher.seal(&self.data)?;
        self.generation = DurableBlobStore::new(self.directory.join("wallet"))
            .save(Some(self.generation), &bytes)
            .map_err(|error| format!("cannot persist wallet: {error:?}"))?;
        Ok(())
    }
}

pub(super) fn password(
    file: Option<&str>,
    confirmation: bool,
) -> Result<Zeroizing<String>, String> {
    if let Some(path) = file {
        let path = Path::new(path);
        local_file::validate_private_file_permissions(path)?;
        let mut bytes = Zeroizing::new(local_file::read_bounded(
            path,
            4096,
            "wallet password file",
        )?);
        let password = std::str::from_utf8(&bytes)
            .map_err(|_| "password file must be UTF-8")?
            .trim_end_matches(['\r', '\n'])
            .to_owned();
        bytes.zeroize();
        if password.is_empty() {
            return Err("wallet password must not be empty".to_owned());
        }
        return Ok(Zeroizing::new(password));
    }
    let first = Zeroizing::new(
        rpassword::prompt_password("Wallet password: ").map_err(|error| error.to_string())?,
    );
    if confirmation {
        let second = Zeroizing::new(
            rpassword::prompt_password("Repeat password: ").map_err(|error| error.to_string())?,
        );
        if *first != *second {
            return Err("wallet passwords do not match".to_owned());
        }
    }
    if first.is_empty() {
        return Err("wallet password must not be empty".to_owned());
    }
    Ok(first)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Backup {
    version: u32,
    wallet_base64: String,
    public_slots_base64: Vec<String>,
}

pub(super) fn backup(session: &WalletSession, destination: &Path) -> Result<(), String> {
    use base64::Engine as _;
    let encode = &base64::engine::general_purpose::STANDARD;
    let (_, bytes) = DurableBlobStore::new(session.directory.join("wallet"))
        .load()
        .map_err(|error| format!("backup load: {error:?}"))?
        .ok_or("wallet missing")?;
    let slots = ["node.a", "node.b", "node.commit"]
        .iter()
        .map(|name| {
            local_file::read_bounded(
                &session.directory.join(name),
                MAX_FILE,
                "wallet public state",
            )
            .map(|bytes| encode.encode(bytes))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let bytes = serde_json::to_vec(&Backup {
        version: 1,
        wallet_base64: encode.encode(bytes),
        public_slots_base64: slots,
    })
    .map_err(|error| error.to_string())?;
    local_file::write_new_private(destination, &bytes, "encrypted wallet backup")
}

pub(super) fn restore(source: &Path, directory: &Path, password: &str) -> Result<(), String> {
    use base64::Engine as _;
    let decode = &base64::engine::general_purpose::STANDARD;
    let backup: Backup = serde_json::from_slice(&local_file::read_bounded(
        source,
        4 * MAX_FILE,
        "wallet backup",
    )?)
    .map_err(|error| error.to_string())?;
    if backup.version != 1 || backup.public_slots_base64.len() != 3 {
        return Err("invalid wallet backup".to_owned());
    }
    let wallet = decode
        .decode(&backup.wallet_base64)
        .map_err(|_| "invalid backup wallet encoding")?;
    let (_, data) = VaultCipher::open(password, &wallet)?;
    let slots = backup
        .public_slots_base64
        .iter()
        .map(|bytes| {
            decode
                .decode(bytes)
                .map_err(|_| "invalid backup trust encoding")
        })
        .collect::<Result<Vec<_>, _>>()?;
    std::fs::create_dir(directory)
        .map_err(|error| format!("restore requires a new directory: {error}"))?;
    for (name, bytes) in ["node.a", "node.b", "node.commit"].iter().zip(&slots) {
        local_file::write_new(&directory.join(name), bytes, "wallet trust snapshot")?;
    }
    second::PublicStateStore::new(directory.join("node"))
        .load()
        .map_err(|error| format!("invalid backup trust state: {error:?}"))?
        .ok_or("backup trust missing")?;
    WalletSession::create(directory, data, password)?;
    Ok(())
}
