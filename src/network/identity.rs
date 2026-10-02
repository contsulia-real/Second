use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ed25519_dalek::pkcs8::DecodePrivateKey;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519, PublicKeyData};
use rustls::pki_types::PrivatePkcs8KeyDer;

use super::{CURRENT_NETWORK_PROTOCOL_VERSION, NetworkError, NodeId};
use crate::network::quic::SECOND_QUIC_SERVER_NAME;

const TRANSPORT_IDENTITY_MAGIC: &[u8; 8] = b"S2TIDV1\0";
const MAX_TRANSPORT_PRIVATE_KEY_SIZE: usize = 4096;
const PEER_AUTH_DOMAIN: &[u8] = b"SECOND_QUIC_PEER_AUTH_V1\0";

#[derive(Clone, Copy)]
pub(crate) enum PeerAuthRole {
    Client = 1,
    Server = 2,
}

#[derive(Clone)]
pub struct QuicTransportIdentity {
    certificate_der: Vec<u8>,
    private_key_der: Vec<u8>,
    signing_key: SigningKey,
    node_id: NodeId,
}

pub fn transport_identity_path(snapshot_base: &Path) -> PathBuf {
    append_suffix(snapshot_base, ".transport")
}

impl QuicTransportIdentity {
    pub fn generate() -> Result<Self, NetworkError> {
        let key_pair = KeyPair::generate_for(&PKCS_ED25519).map_err(identity_error)?;
        Self::from_key_pair(key_pair)
    }

    pub fn load_or_generate(path: impl AsRef<Path>) -> Result<Self, NetworkError> {
        let path = path.as_ref();

        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(identity_error)?;
        }

        let lock_path = append_suffix(path, ".lock");
        let lock_file = open_identity_lock(&lock_path).map_err(identity_error)?;
        File::lock(&lock_file).map_err(identity_error)?;

        match fs::read(path) {
            Ok(bytes) => return Self::decode_file(&bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(identity_error(error)),
        }

        let identity = Self::generate()?;
        let encoded = identity.encode_file()?;
        let staging_path = append_suffix(path, ".new");
        write_staged_identity(&staging_path, &encoded).map_err(identity_error)?;

        if let Err(error) = fs::rename(&staging_path, path) {
            let _ = fs::remove_file(&staging_path);
            return Err(identity_error(error));
        }

        sync_parent_directory(path).map_err(identity_error)?;
        Ok(identity)
    }

    pub const fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }

    pub(crate) fn private_key_der(&self) -> &[u8] {
        &self.private_key_der
    }

    pub(crate) fn sign_peer_auth(
        &self,
        channel_binding: &[u8; 32],
        role: PeerAuthRole,
    ) -> [u8; 64] {
        self.signing_key
            .sign(&peer_auth_message(channel_binding, role))
            .to_bytes()
    }

    pub(crate) fn verify_peer_auth(
        node_id: NodeId,
        signature: [u8; 64],
        channel_binding: &[u8; 32],
        role: PeerAuthRole,
    ) -> Result<(), NetworkError> {
        let verifying_key = VerifyingKey::from_bytes(&node_id.to_bytes())
            .map_err(|_| NetworkError::PeerAuthenticationFailed(node_id))?;
        let signature = Signature::from_bytes(&signature);

        verifying_key
            .verify(&peer_auth_message(channel_binding, role), &signature)
            .map_err(|_| NetworkError::PeerAuthenticationFailed(node_id))
    }

    fn from_private_key_der(private_key_der: &[u8]) -> Result<Self, NetworkError> {
        let key_der = PrivatePkcs8KeyDer::from(private_key_der.to_vec());
        let key_pair = KeyPair::from_pkcs8_der_and_sign_algo(&key_der, &PKCS_ED25519)
            .map_err(identity_error)?;
        Self::from_key_pair(key_pair)
    }

    fn from_key_pair(key_pair: KeyPair) -> Result<Self, NetworkError> {
        let public_key: [u8; 32] = key_pair
            .der_bytes()
            .try_into()
            .map_err(|_| NetworkError::InvalidTransportIdentity)?;
        let private_key_der = key_pair.serialize_der();
        let signing_key = SigningKey::from_pkcs8_der(&private_key_der).map_err(identity_error)?;
        if signing_key.verifying_key().to_bytes() != public_key {
            return Err(NetworkError::InvalidTransportIdentity);
        }

        let params = CertificateParams::new(vec![SECOND_QUIC_SERVER_NAME.to_owned()])
            .map_err(identity_error)?;
        let certificate_der = params
            .self_signed(&key_pair)
            .map_err(identity_error)?
            .der()
            .to_vec();

        Ok(Self {
            certificate_der,
            private_key_der,
            signing_key,
            node_id: NodeId::from_bytes(public_key),
        })
    }

    fn encode_file(&self) -> Result<Vec<u8>, NetworkError> {
        let key_len = u32::try_from(self.private_key_der.len())
            .map_err(|_| NetworkError::InvalidTransportIdentity)?;
        let mut encoded =
            Vec::with_capacity(TRANSPORT_IDENTITY_MAGIC.len() + 4 + self.private_key_der.len());
        encoded.extend_from_slice(TRANSPORT_IDENTITY_MAGIC);
        encoded.extend_from_slice(&key_len.to_be_bytes());
        encoded.extend_from_slice(&self.private_key_der);
        Ok(encoded)
    }

    fn decode_file(bytes: &[u8]) -> Result<Self, NetworkError> {
        if bytes.len() < TRANSPORT_IDENTITY_MAGIC.len() + 4
            || &bytes[..TRANSPORT_IDENTITY_MAGIC.len()] != TRANSPORT_IDENTITY_MAGIC
        {
            return Err(NetworkError::InvalidTransportIdentity);
        }

        let key_len = u32::from_be_bytes(
            bytes[TRANSPORT_IDENTITY_MAGIC.len()..TRANSPORT_IDENTITY_MAGIC.len() + 4]
                .try_into()
                .map_err(|_| NetworkError::InvalidTransportIdentity)?,
        ) as usize;
        if key_len == 0 || key_len > MAX_TRANSPORT_PRIVATE_KEY_SIZE {
            return Err(NetworkError::InvalidTransportIdentity);
        }

        let key_start = TRANSPORT_IDENTITY_MAGIC.len() + 4;
        let key_end = key_start
            .checked_add(key_len)
            .ok_or(NetworkError::InvalidTransportIdentity)?;
        if key_end != bytes.len() {
            return Err(NetworkError::InvalidTransportIdentity);
        }

        Self::from_private_key_der(&bytes[key_start..key_end])
    }
}

fn open_identity_lock(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
}

fn write_staged_identity(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn peer_auth_message(channel_binding: &[u8; 32], role: PeerAuthRole) -> Vec<u8> {
    let mut message = Vec::with_capacity(PEER_AUTH_DOMAIN.len() + 4 + 1 + channel_binding.len());
    message.extend_from_slice(PEER_AUTH_DOMAIN);
    message.extend_from_slice(&CURRENT_NETWORK_PROTOCOL_VERSION.to_be_bytes());
    message.push(role as u8);
    message.extend_from_slice(channel_binding);
    message
}

fn identity_error(error: impl std::fmt::Display) -> NetworkError {
    NetworkError::TransportIdentity(error.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_identity_path(label: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "second-transport-{label}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock must be after unix epoch")
                .as_nanos(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed),
        ))
    }

    #[test]
    fn persisted_identity_preserves_node_id_and_certificate_pin() {
        let path = temp_identity_path("identity");

        let first = QuicTransportIdentity::load_or_generate(&path).unwrap();
        let node_id = first.node_id();
        let certificate = first.certificate_der().to_vec();
        drop(first);

        let restored = QuicTransportIdentity::load_or_generate(&path).unwrap();
        assert_eq!(restored.node_id(), node_id);
        assert_eq!(restored.certificate_der(), certificate);

        fs::remove_file(&path).unwrap();
        fs::remove_file(append_suffix(&path, ".lock")).unwrap();
    }

    #[test]
    fn unpublished_staging_file_does_not_replace_or_block_identity_creation() {
        let path = temp_identity_path("staging");
        let staging_path = append_suffix(&path, ".new");
        fs::write(&staging_path, b"partial unpublished identity").unwrap();

        let identity = QuicTransportIdentity::load_or_generate(&path).unwrap();
        let restored = QuicTransportIdentity::load_or_generate(&path).unwrap();
        assert_eq!(restored.node_id(), identity.node_id());
        assert_eq!(restored.certificate_der(), identity.certificate_der());
        assert!(!staging_path.exists());

        fs::remove_file(&path).unwrap();
        fs::remove_file(append_suffix(&path, ".lock")).unwrap();
    }

    #[test]
    fn invalid_published_identity_fails_closed_without_rotation() {
        let path = temp_identity_path("invalid");
        fs::write(&path, b"corrupted published identity").unwrap();

        assert!(matches!(
            QuicTransportIdentity::load_or_generate(&path),
            Err(NetworkError::InvalidTransportIdentity)
        ));
        assert_eq!(fs::read(&path).unwrap(), b"corrupted published identity");

        fs::remove_file(&path).unwrap();
        fs::remove_file(append_suffix(&path, ".lock")).unwrap();
    }

    #[test]
    fn peer_authentication_is_bound_to_connection_and_role() {
        let identity = QuicTransportIdentity::generate().unwrap();
        let first_binding = [1; 32];
        let second_binding = [2; 32];
        let signature = identity.sign_peer_auth(&first_binding, PeerAuthRole::Client);
        let other_identity = QuicTransportIdentity::generate().unwrap();

        assert_eq!(
            QuicTransportIdentity::verify_peer_auth(
                identity.node_id(),
                signature,
                &first_binding,
                PeerAuthRole::Client,
            ),
            Ok(())
        );
        assert!(matches!(
            QuicTransportIdentity::verify_peer_auth(
                other_identity.node_id(),
                signature,
                &first_binding,
                PeerAuthRole::Client,
            ),
            Err(NetworkError::PeerAuthenticationFailed(_))
        ));
        assert!(matches!(
            QuicTransportIdentity::verify_peer_auth(
                identity.node_id(),
                signature,
                &second_binding,
                PeerAuthRole::Client,
            ),
            Err(NetworkError::PeerAuthenticationFailed(_))
        ));
        assert!(matches!(
            QuicTransportIdentity::verify_peer_auth(
                identity.node_id(),
                signature,
                &first_binding,
                PeerAuthRole::Server,
            ),
            Err(NetworkError::PeerAuthenticationFailed(_))
        ));
    }
}
