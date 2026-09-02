use serde::{Deserialize, Serialize};
use worker::{Error, Result, kv::KvStore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheEntry<T> {
    pub data: T,
    pub timestamp: u64,
}

pub struct KvManager {
    store: KvStore,
}

impl KvManager {
    pub fn new(store: KvStore) -> Self {
        Self { store }
    }

    pub async fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.store
            .put(key, serde_json::to_string(value)?)?
            .execute()
            .await
            .map_err(|error| Error::RustError(error.to_string()))
    }

    pub async fn get<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>> {
        match self.store.get(key).json::<T>().await? {
            Some(value) => Ok(Some(value)),
            None => Ok(None),
        }
    }

    pub async fn put<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.set(key, value).await
    }

    pub async fn delete(&self, key: &str) -> Result<()> {
        self.store
            .delete(key)
            .await
            .map_err(|error| Error::RustError(error.to_string()))
    }

    pub async fn list(&self, prefix: Option<&str>) -> Result<Vec<String>> {
        let mut request = self.store.list().limit(100);
        if let Some(prefix) = prefix {
            request = request.prefix(prefix.to_string());
        }

        let list = request
            .execute()
            .await
            .map_err(|error| Error::RustError(error.to_string()))?;
        Ok(list.keys.iter().map(|k| k.name.clone()).collect())
    }

    pub async fn list_keys(&self, prefix: &str) -> Result<Vec<String>> {
        self.list(Some(prefix)).await
    }

    pub async fn get_with_ttl<T: for<'de> Deserialize<'de>>(
        &self,
        key: &str,
        ttl_seconds: u64,
    ) -> Result<Option<T>> {
        match self.store.get(key).json::<CacheEntry<T>>().await? {
            Some(entry) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();

                if now - entry.timestamp < ttl_seconds {
                    Ok(Some(entry.data))
                } else {
                    self.delete(key).await?;
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    pub async fn put_with_ttl<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        ttl_seconds: u64,
    ) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let entry = CacheEntry {
            data: value,
            timestamp: now,
        };

        self.set_with_ttl(key, &entry, ttl_seconds).await
    }

    pub async fn set_with_ttl<T: Serialize>(
        &self,
        key: &str,
        value: &T,
        ttl_seconds: u64,
    ) -> Result<()> {
        self.store
            .put(key, serde_json::to_string(value)?)?
            .expiration_ttl(ttl_seconds)
            .execute()
            .await
            .map_err(|error| Error::RustError(error.to_string()))
    }
}

pub type KVManager = KvManager;
