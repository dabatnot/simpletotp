use anyhow::{Context, Result, bail};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE},
};
use data_encoding::BASE32_NOPAD;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use totp_rs::{Algorithm, Builder, Totp};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: Uuid,
    pub label: String,
    pub totp: Totp,
}

impl Account {
    pub fn from_key(service: &str, name: &str, key: &str) -> Result<Self> {
        let service = service.trim();
        let name = name.trim();
        if service.is_empty() || name.is_empty() {
            bail!("Indiquez un service et un compte.");
        }
        let normalized: String = key
            .chars()
            .filter(|c| !c.is_ascii_whitespace() && *c != '-')
            .collect::<String>()
            .to_ascii_uppercase();
        let secret = BASE32_NOPAD
            .decode(normalized.trim_end_matches('=').as_bytes())
            .context("Clé Base32 invalide.")?;
        let totp = Builder::new()
            .with_secret(secret)
            .with_issuer(Some(service))
            .with_account_name(name)
            .build()
            .context("Clé TOTP invalide.")?;
        Ok(Self::new(totp))
    }

    pub fn from_url(uri: &str) -> Result<Self> {
        if !uri.starts_with("otpauth://totp/") {
            bail!("Ce QR code ne contient pas un compte TOTP.");
        }
        let totp = Totp::from_url(uri).context("Adresse TOTP invalide.")?;
        Ok(Self::new(totp))
    }

    fn new(totp: Totp) -> Self {
        let label = match totp.issuer() {
            Some(issuer) if !issuer.is_empty() => format!("{issuer} · {}", totp.account_name()),
            _ => totp.account_name().to_owned(),
        };
        Self {
            id: Uuid::new_v4(),
            label,
            totp,
        }
    }

    pub fn code(&self) -> String {
        self.totp.generate_current().to_string()
    }
    pub fn qr_png(&self) -> Result<Vec<u8>> {
        self.totp.to_qr_png().map_err(|e| anyhow::anyhow!(e))
    }
}

#[derive(Clone, PartialEq, Message)]
struct MigrationPayload {
    #[prost(message, repeated, tag = "1")]
    accounts: Vec<MigrationAccount>,
    #[prost(int32, tag = "2")]
    version: i32,
    #[prost(int32, tag = "3")]
    batch_size: i32,
    #[prost(int32, tag = "4")]
    batch_index: i32,
    #[prost(int32, tag = "5")]
    batch_id: i32,
}

#[derive(Clone, PartialEq, Message)]
struct MigrationAccount {
    #[prost(bytes, tag = "1")]
    secret: Vec<u8>,
    #[prost(string, tag = "2")]
    name: String,
    #[prost(string, tag = "3")]
    issuer: String,
    #[prost(int32, tag = "4")]
    algorithm: i32,
    #[prost(int32, tag = "5")]
    digits: i32,
    #[prost(int32, tag = "6")]
    kind: i32,
}

pub enum ImportResult {
    Pending(usize, usize),
    Ready(Vec<Account>, usize),
}

#[derive(Default)]
pub struct ImportSession {
    batch_id: Option<i32>,
    parts: BTreeMap<i32, Vec<MigrationAccount>>,
    batch_size: i32,
}

impl ImportSession {
    pub fn ingest(&mut self, text: &str) -> Result<ImportResult> {
        if text.starts_with("otpauth://") {
            return Ok(ImportResult::Ready(vec![Account::from_url(text)?], 0));
        }
        let uri = url::Url::parse(text).context("QR code inconnu.")?;
        if uri.scheme() != "otpauth-migration" || uri.host_str() != Some("offline") {
            bail!("QR code inconnu.");
        }
        let data = uri
            .query_pairs()
            .find(|(k, _)| k == "data")
            .map(|(_, v)| v.into_owned())
            .context("Export Google incomplet.")?;
        let bytes = STANDARD
            .decode(&data)
            .or_else(|_| URL_SAFE.decode(&data))
            .context("Export Google invalide.")?;
        let payload =
            MigrationPayload::decode(bytes.as_slice()).context("Export Google illisible.")?;
        if payload.batch_size < 1
            || payload.batch_size > 100
            || payload.batch_index < 0
            || payload.batch_index >= payload.batch_size
            || payload.accounts.is_empty()
        {
            bail!("Export Google incomplet.");
        }
        if self.batch_id != Some(payload.batch_id) || self.batch_size != payload.batch_size {
            self.parts.clear();
            self.batch_id = Some(payload.batch_id);
            self.batch_size = payload.batch_size;
        }
        self.parts.insert(payload.batch_index, payload.accounts);
        if self.parts.len() != payload.batch_size as usize {
            return Ok(ImportResult::Pending(
                self.parts.len(),
                payload.batch_size as usize,
            ));
        }
        let mut accounts = Vec::new();
        let mut skipped = 0;
        for part in self.parts.values() {
            for entry in part {
                if entry.kind != 2 {
                    skipped += 1;
                    continue;
                }
                let algorithm = match entry.algorithm {
                    0 | 1 => Algorithm::SHA1,
                    2 => Algorithm::SHA256,
                    3 => Algorithm::SHA512,
                    _ => {
                        skipped += 1;
                        continue;
                    }
                };
                let digits = match entry.digits {
                    0 | 1 => 6,
                    2 => 8,
                    _ => {
                        skipped += 1;
                        continue;
                    }
                };
                let mut builder = Builder::new()
                    .with_secret(entry.secret.clone())
                    .with_algorithm(algorithm)
                    .with_digits(digits)
                    .with_account_name(entry.name.as_str());
                if !entry.issuer.is_empty() {
                    builder = builder.with_issuer(Some(entry.issuer.as_str()));
                }
                match builder.build() {
                    Ok(totp) => accounts.push(Account::new(totp)),
                    Err(_) => skipped += 1,
                }
            }
        }
        self.parts.clear();
        self.batch_id = None;
        Ok(ImportResult::Ready(accounts, skipped))
    }
}

pub fn read_qr_image(bytes: &[u8]) -> Result<Vec<String>> {
    let image = image::load_from_memory(bytes)
        .context("Image illisible.")?
        .to_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(image);
    let values: Vec<String> = prepared
        .detect_grids()
        .into_iter()
        .filter_map(|grid| grid.decode().ok().map(|(_, value)| value))
        .collect();
    if values.is_empty() {
        bail!("Aucun QR code lisible dans cette image.");
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_6238_vectors() {
        for (algorithm, secret, expected) in [
            (
                Algorithm::SHA1,
                b"12345678901234567890".as_slice(),
                "94287082",
            ),
            (
                Algorithm::SHA256,
                b"12345678901234567890123456789012".as_slice(),
                "46119246",
            ),
            (
                Algorithm::SHA512,
                b"1234567890123456789012345678901234567890123456789012345678901234".as_slice(),
                "90693936",
            ),
        ] {
            let totp = Builder::new()
                .with_secret(secret.to_vec())
                .with_algorithm(algorithm)
                .with_digits(8)
                .build()
                .unwrap();
            assert_eq!(totp.generate(59).to_string(), expected);
        }
    }

    #[test]
    fn migration_assembles_pages() {
        let part = |index| MigrationPayload {
            accounts: vec![MigrationAccount {
                secret: b"12345678901234567890".to_vec(),
                name: format!("user{index}"),
                issuer: "Example".into(),
                algorithm: 1,
                digits: 1,
                kind: 2,
            }],
            version: 1,
            batch_size: 2,
            batch_index: index,
            batch_id: 42,
        };
        let uri = |p: MigrationPayload| {
            format!(
                "otpauth-migration://offline?data={}",
                url::form_urlencoded::byte_serialize(STANDARD.encode(p.encode_to_vec()).as_bytes())
                    .collect::<String>()
            )
        };
        let mut session = ImportSession::default();
        assert!(matches!(
            session.ingest(&uri(part(0))).unwrap(),
            ImportResult::Pending(1, 2)
        ));
        match session.ingest(&uri(part(1))).unwrap() {
            ImportResult::Ready(accounts, 0) => assert_eq!(accounts.len(), 2),
            _ => panic!("expected two accounts"),
        }
    }

    #[test]
    fn qr_round_trip_and_serialization() {
        let account =
            Account::from_key("Example", "alice", "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ").unwrap();
        let values = read_qr_image(&account.qr_png().unwrap()).unwrap();
        let imported = Account::from_url(&values[0]).unwrap();
        assert_eq!(imported.totp, account.totp);
        let stored: Account =
            serde_json::from_slice(&serde_json::to_vec(&account).unwrap()).unwrap();
        assert_eq!(stored.totp, account.totp);
    }
}
