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
}
