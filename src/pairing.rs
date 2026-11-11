use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::Duration;

const WORDS: &str = include_str!("../assets/wordlist.txt");
const WORD_COUNT: usize = 6;
const PBKDF2_ROUNDS: u32 = 600_000;
const AAD: &[u8] = b"corcho-pair";

/// Upload a setup code under a six-word pairing code and return that code.
pub fn create(setup: &str) -> Result<String> {
    let relay =
        crate::irohsync::relay_base_url().ok_or_else(|| anyhow!("set a relay endpoint first"))?;
    let words: Vec<&str> = WORDS.lines().filter(|word| !word.is_empty()).collect();
    if words.len() != 2048 {
        return Err(anyhow!("word list is incomplete"));
    }
    let code = (0..WORD_COUNT)
        .map(|_| words[rand::random_range(0..words.len())])
        .collect::<Vec<_>>()
        .join("-");
    let normalized = normalize(&code);
    let salt: [u8; 16] = rand::random();
    let key = derive(&normalized, &salt)?;
    let nonce: [u8; 24] = rand::random();
    let ciphertext = encrypt(&key, nonce, setup.as_bytes())?;
    let blob = json!({
        "v": 1,
        "salt": URL_SAFE_NO_PAD.encode(salt),
        "nonce": URL_SAFE_NO_PAD.encode(nonce),
        "c": URL_SAFE_NO_PAD.encode(ciphertext),
    })
    .to_string();
    post(
        &format!("{relay}/pair"),
        &json!({"lookup": lookup(&normalized), "blob": blob}),
    )?;
    Ok(code)
}

/// Fetch and decrypt a setup code for a pairing code.
pub fn take(code: &str) -> Result<String> {
    let relay =
        crate::irohsync::relay_base_url().ok_or_else(|| anyhow!("set a relay endpoint first"))?;
    let normalized = normalize(code);
    let parts: Vec<&str> = normalized
        .split('-')
        .filter(|part| !part.is_empty())
        .collect();
    if parts.len() != WORD_COUNT {
        return Err(anyhow!("enter the complete {WORD_COUNT}-word code"));
    }
    let words: Vec<&str> = WORDS.lines().filter(|word| !word.is_empty()).collect();
    if parts.iter().any(|part| words.binary_search(part).is_err()) {
        return Err(anyhow!("one of the words is not in the word list"));
    }
    let response = post(
        &format!("{relay}/pair/take"),
        &json!({"lookup": lookup(&normalized)}),
    )?;
    let blob = response
        .get("blob")
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("pairing response is missing the payload"))?;
    let value: Value = serde_json::from_str(blob)?;
    let salt = decode(value.get("salt"))?;
    let nonce = decode(value.get("nonce"))?;
    let ciphertext = decode(value.get("c"))?;
    let key = derive(&normalized, &salt)?;
    let plaintext = decrypt(&key, &nonce, &ciphertext)?;
    String::from_utf8(plaintext).map_err(|_| anyhow!("pairing payload is not text"))
}

fn normalize(code: &str) -> String {
    code.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("-")
}

fn lookup(normalized: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"corcho-pair-lookup\0");
    hasher.update(normalized.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn derive(code: &str, salt: &[u8]) -> Result<[u8; 32]> {
    if salt.len() != 16 {
        return Err(anyhow!("invalid pairing salt"));
    }
    let mut key = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(code.as_bytes(), salt, PBKDF2_ROUNDS, &mut key);
    Ok(key)
}

fn encrypt(key: &[u8; 32], nonce: [u8; 24], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(key));
    cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: AAD,
            },
        )
        .map_err(|_| anyhow!("cannot encrypt pairing payload"))
}

fn decrypt(key: &[u8; 32], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if nonce.len() != 24 {
        return Err(anyhow!("invalid pairing nonce"));
    }
    let cipher = XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(key));
    cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: AAD,
            },
        )
        .map_err(|_| anyhow!("cannot decrypt pairing payload"))
}

fn decode(value: Option<&Value>) -> Result<Vec<u8>> {
    let text = value
        .and_then(|value| value.as_str())
        .ok_or_else(|| anyhow!("pairing payload is incomplete"))?;
    URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|err| anyhow!("invalid pairing payload: {err}"))
}

fn post(url: &str, body: &Value) -> Result<Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()?;
    let response = client
        .post(url)
        .json(body)
        .send()
        .with_context(|| format!("pairing service {url}"))?;
    let status = response.status();
    let text = response.text()?;
    if !status.is_success() {
        return Err(anyhow!("pairing service: {status}"));
    }
    serde_json::from_str(&text).map_err(|err| anyhow!("pairing response: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wordlist_is_complete() {
        assert_eq!(WORDS.lines().filter(|word| !word.is_empty()).count(), 2048);
    }

    #[test]
    fn normalization_accepts_spaces_and_case() {
        assert_eq!(normalize("  Abandon ability-ABLE "), "abandon-ability-able");
    }

    #[test]
    fn pairing_payload_roundtrips_and_binds_code() {
        let code = normalize("abandon-ability-able-about-above-absent");
        let salt = [7u8; 16];
        let key = derive(&code, &salt).unwrap();
        let nonce = [9u8; 24];
        let sealed = encrypt(&key, nonce, b"corcho2:test").unwrap();
        assert_eq!(decrypt(&key, &nonce, &sealed).unwrap(), b"corcho2:test");
        let wrong = derive(&normalize("zoo-zoo-zoo-zoo-zoo-zoo"), &salt).unwrap();
        assert!(decrypt(&wrong, &nonce, &sealed).is_err());
    }
}
