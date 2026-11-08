use crate::otp::Account;
use anyhow::{Context, Result};
use futures_lite::future::block_on;
use secret_service::{EncryptionType, SecretService};
use std::collections::HashMap;
use uuid::Uuid;

const APP: &str = "simpletotp";

pub fn load() -> Result<Vec<Account>> {
    block_on(async {
        let service = SecretService::connect(EncryptionType::Dh)
            .await
            .context("Coffre de secrets indisponible")?;
        let found = service
            .search_items(HashMap::from([("application", APP)]))
            .await?;
        let mut accounts = Vec::new();
        for item in found.unlocked.into_iter().chain(found.locked) {
            if item.is_locked().await? {
                item.unlock()
                    .await
                    .context("Impossible de déverrouiller le coffre")?;
            }
            let bytes = item.get_secret().await?;
            accounts.push(serde_json::from_slice(&bytes).context("Compte enregistré illisible")?);
        }
        accounts.sort_by_key(|account: &Account| account.label.to_lowercase());
        Ok(accounts)
    })
}

pub fn save(account: &Account) -> Result<()> {
    block_on(async {
        let service = SecretService::connect(EncryptionType::Dh)
            .await
            .context("Coffre de secrets indisponible")?;
        let collection = service.get_default_collection().await?;
        if collection.is_locked().await? {
            collection.unlock().await?;
        }
        let id = account.id.to_string();
        let attributes = HashMap::from([("application", APP), ("id", id.as_str())]);
        let bytes = serde_json::to_vec(account)?;
        collection
            .create_item("SimpleTOTP", attributes, &bytes, true, "application/json")
            .await?;
        Ok(())
    })
}

pub fn delete(id: Uuid) -> Result<()> {
    block_on(async {
        let service = SecretService::connect(EncryptionType::Dh)
            .await
            .context("Coffre de secrets indisponible")?;
        let id = id.to_string();
        let found = service
            .search_items(HashMap::from([("application", APP), ("id", id.as_str())]))
            .await?;
        for item in found.unlocked.into_iter().chain(found.locked) {
            if item.is_locked().await? {
                item.unlock().await?;
            }
            item.delete().await?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs a desktop Secret Service session"]
    fn live_keyring_round_trip() {
        let account = Account::from_key(
            "SimpleTOTP test",
            "temporary",
            "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ",
        )
        .unwrap();
        save(&account).unwrap();
        let found = load().unwrap().iter().any(|saved| saved.id == account.id);
        delete(account.id).unwrap();
        assert!(found);
        assert!(!load().unwrap().iter().any(|saved| saved.id == account.id));
    }

    #[test]
    #[ignore = "needs a desktop Secret Service session"]
    fn live_saved_qrs_round_trip_without_changes() {
        let accounts = load().unwrap();
        assert!(!accounts.is_empty(), "no saved accounts found");
        for (index, account) in accounts.iter().enumerate() {
            let values = crate::otp::read_qr_image(&account.qr_png().unwrap()).unwrap();
            assert_eq!(values.len(), 1, "unexpected QR count at index {index}");
            let imported = Account::from_url(&values[0]).unwrap();
            assert!(
                imported.totp == account.totp,
                "QR mismatch at index {index}"
            );
        }
        let images = crate::otp::google_export_qrs(&accounts).unwrap();
        let mut session = crate::otp::ImportSession::default();
        let mut restored = None;
        for image in &images {
            let values = crate::otp::read_qr_image(image).unwrap();
            assert_eq!(values.len(), 1);
            match session.ingest(&values[0]).unwrap() {
                crate::otp::ImportResult::Pending(_, _) => {}
                crate::otp::ImportResult::Ready(imported, skipped) => {
                    assert_eq!(skipped, 0);
                    restored = Some(imported);
                }
            }
        }
        let restored = restored.expect("grouped import incomplete");
        assert_eq!(restored.len(), accounts.len());
        assert!(
            restored
                .iter()
                .zip(&accounts)
                .all(|(a, b)| a.totp == b.totp)
        );
        eprintln!(
            "Verified {} saved accounts in {} grouped QR codes without changing the keyring",
            accounts.len(),
            images.len()
        );
    }
}
