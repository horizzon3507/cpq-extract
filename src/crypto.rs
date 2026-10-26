use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use aes::Aes256;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use pbkdf2::pbkdf2_hmac;
use sha1::Sha1;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::Error;

const PASSWORD_SALT: &[u8] = b"copyq_salt_v1";
const PASSWORD_ITERATIONS: u32 = 100_000;
const ENVELOPE_MAGIC: &[u8] = b"CPQ1";
const MESSAGE_KEY_INFO: &[u8] = b"copyq-msg-key-v1";
const HMAC_KEY_INFO: &[u8] = b"copyq-hmac-v1";
const SALT_LENGTH: usize = 16;
const AES_BLOCK_LENGTH: usize = 16;
const AUTH_TAG_LENGTH: usize = 32;
const ENVELOPE_HEADER_LENGTH: usize = ENVELOPE_MAGIC.len() + SALT_LENGTH + AES_BLOCK_LENGTH;
const MIN_ENVELOPE_LENGTH: usize = ENVELOPE_HEADER_LENGTH + AES_BLOCK_LENGTH + AUTH_TAG_LENGTH;

type Aes256CbcDec = cbc::Decryptor<Aes256>;

pub struct EncryptionKey(Zeroizing<[u8; 32]>);

impl EncryptionKey {
    pub fn from_password(password: &str) -> Self {
        let mut key = Zeroizing::new([0_u8; 32]);
        pbkdf2_hmac::<Sha1>(
            password.as_bytes(),
            PASSWORD_SALT,
            PASSWORD_ITERATIONS,
            &mut *key,
        );
        Self(key)
    }

    fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

pub fn decrypt_envelope(payload: &[u8], key: &EncryptionKey) -> Result<Vec<u8>, Error> {
    if payload.len() < MIN_ENVELOPE_LENGTH || !payload.starts_with(ENVELOPE_MAGIC) {
        return Err(Error::InvalidData(
            "encrypted block does not have a valid CopyQ envelope".into(),
        ));
    }

    let salt_start = ENVELOPE_MAGIC.len();
    let iv_start = salt_start + SALT_LENGTH;
    let ciphertext_start = iv_start + AES_BLOCK_LENGTH;
    let signed_end = payload.len() - AUTH_TAG_LENGTH;
    let salt = &payload[salt_start..iv_start];
    let message_hkdf = Hkdf::<Sha256>::new(Some(salt), key.bytes());
    let mut message_key = Zeroizing::new([0_u8; 32]);
    message_hkdf
        .expand(MESSAGE_KEY_INFO, &mut *message_key)
        .map_err(|_| Error::InvalidData("failed to derive CopyQ message key".into()))?;

    let zero_salt = [0_u8; 32];
    let hmac_hkdf = Hkdf::<Sha256>::new(Some(&zero_salt), &*message_key);
    let mut hmac_key = Zeroizing::new([0_u8; 32]);
    hmac_hkdf
        .expand(HMAC_KEY_INFO, &mut *hmac_key)
        .map_err(|_| Error::InvalidData("failed to derive CopyQ authentication key".into()))?;

    let mut authenticator = Hmac::<Sha256>::new_from_slice(&*hmac_key)
        .map_err(|_| Error::InvalidData("invalid CopyQ authentication key".into()))?;
    authenticator.update(&payload[..signed_end]);
    authenticator
        .verify_slice(&payload[signed_end..])
        .map_err(|_| Error::InvalidData("wrong password or corrupted encrypted block".into()))?;

    let iv: &[u8; AES_BLOCK_LENGTH] = payload[iv_start..ciphertext_start]
        .try_into()
        .map_err(|_| Error::InvalidData("encrypted block has an invalid AES IV".into()))?;
    let ciphertext = &payload[ciphertext_start..signed_end];
    if ciphertext.is_empty() || ciphertext.len() % AES_BLOCK_LENGTH != 0 {
        return Err(Error::InvalidData(
            "encrypted block has invalid AES ciphertext length".into(),
        ));
    }

    let decryptor = Aes256CbcDec::new_from_slices(&*message_key, iv)
        .map_err(|_| Error::InvalidData("could not initialize AES-256-CBC".into()))?;
    decryptor
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .map_err(|_| Error::InvalidData("wrong password or corrupted encrypted block".into()))
}

#[cfg(test)]
mod tests {
    use super::{decrypt_envelope, EncryptionKey};

    #[test]
    fn decrypts_copyq_authenticated_envelope() {
        let envelope = decode_hex(concat!(
            "43505131000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
            "34efa74c3da60729bd80cb26a8c0fb86",
            "8b988c7f4e787d2da30be20970c89e0c",
            "f3c62a036deca6a89fbf970ecd656b0e",
            "572a7a73403f690dff7aca8ae0b04ddf"
        ));
        let key = EncryptionKey::from_password("fixture pass");
        let plaintext = decrypt_envelope(&envelope, &key).unwrap();
        assert_eq!(plaintext, b"CopyQ v5 crypto fixture");
    }

    #[test]
    fn rejects_wrong_password_and_modified_tag() {
        let envelope = decode_hex(concat!(
            "43505131000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
            "34efa74c3da60729bd80cb26a8c0fb86",
            "8b988c7f4e787d2da30be20970c89e0c",
            "f3c62a036deca6a89fbf970ecd656b0e",
            "572a7a73403f690dff7aca8ae0b04ddf"
        ));
        let wrong_key = EncryptionKey::from_password("incorrect");
        assert!(decrypt_envelope(&envelope, &wrong_key).is_err());

        let mut modified = envelope;
        let last = modified.len() - 1;
        modified[last] ^= 1;
        let key = EncryptionKey::from_password("fixture pass");
        assert!(decrypt_envelope(&modified, &key).is_err());
    }

    fn decode_hex(value: &str) -> Vec<u8> {
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let high = (pair[0] as char).to_digit(16).unwrap() as u8;
                let low = (pair[1] as char).to_digit(16).unwrap() as u8;
                high << 4 | low
            })
            .collect()
    }
}
