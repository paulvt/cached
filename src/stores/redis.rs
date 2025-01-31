use crate::IOCached;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::fmt::Display;
use std::marker::PhantomData;

pub struct RedisCacheBuilder<K, V> {
    seconds: u64,
    refresh: bool,
    prefix: String,
    connection_string: Option<String>,
    _phantom_k: PhantomData<K>,
    _phantom_v: PhantomData<V>,
}

const ENV_KEY: &str = "CACHED_REDIS_CONNECTION_STRING";
const PREFIX_NAMESPACE: &str = "cached-redis-store";

use thiserror::Error;

#[derive(Error, Debug)]
pub enum RedisCacheBuildError {
    #[error("redis connection error")]
    Connection(#[from] redis::RedisError),
    #[error("redis pool error")]
    Pool(#[from] r2d2::Error),
    #[error("Connection string not specified or invalid in env var {env_key:?}: {error:?}")]
    MissingConnectionString {
        env_key: String,
        error: std::env::VarError,
    },
}

fn generate_prefix(prefix: &str) -> String {
    format!("{}:{}", PREFIX_NAMESPACE, prefix)
}

impl<K, V> RedisCacheBuilder<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    /// Initialize a `RedisCacheBuilder`
    pub fn new<S: AsRef<str>>(prefix: S, seconds: u64) -> RedisCacheBuilder<K, V> {
        Self {
            seconds,
            refresh: false,
            prefix: generate_prefix(prefix.as_ref()),
            connection_string: None,
            _phantom_k: Default::default(),
            _phantom_v: Default::default(),
        }
    }

    /// Specify the cache TTL/lifespan in seconds
    pub fn set_lifespan(mut self, seconds: u64) -> Self {
        self.seconds = seconds;
        self
    }

    /// Specify whether cache hits refresh the TTL
    pub fn set_refresh(mut self, refresh: bool) -> Self {
        self.refresh = refresh;
        self
    }

    /// Set the prefix for the keys
    pub fn set_prefix<S: AsRef<str>>(mut self, prefix: S) -> Self {
        self.prefix = generate_prefix(prefix.as_ref());
        self
    }

    /// Set the connection string for redis
    pub fn set_connection_string(mut self, cs: &str) -> Self {
        self.connection_string = Some(cs.to_string());
        self
    }

    /// Return the current connection string or load from the env var: CACHED_REDIS_CONNECTION_STRING
    pub fn connection_string(&self) -> Result<String, RedisCacheBuildError> {
        match self.connection_string {
            Some(ref s) => Ok(s.to_string()),
            None => {
                std::env::var(ENV_KEY).map_err(|e| RedisCacheBuildError::MissingConnectionString {
                    env_key: ENV_KEY.to_string(),
                    error: e,
                })
            }
        }
    }

    fn create_pool(&self) -> Result<r2d2::Pool<redis::Client>, RedisCacheBuildError> {
        let s = self.connection_string()?;
        let client: redis::Client = redis::Client::open(s)?;
        let pool: r2d2::Pool<redis::Client> = r2d2::Pool::builder().build(client)?;
        Ok(pool)
    }

    pub fn build(self) -> Result<RedisCache<K, V>, RedisCacheBuildError> {
        Ok(RedisCache {
            seconds: self.seconds,
            refresh: self.refresh,
            connection_string: self.connection_string()?,
            pool: self.create_pool()?,
            prefix: self.prefix,
            _phantom_k: self._phantom_k,
            _phantom_v: self._phantom_v,
        })
    }
}

/// Cache store backed by redis
///
/// Values have a ttl applied and enforced by redis.
pub struct RedisCache<K, V> {
    pub(super) seconds: u64,
    pub(super) refresh: bool,
    pub(super) prefix: String,
    connection_string: String,
    pool: r2d2::Pool<redis::Client>,
    _phantom_k: PhantomData<K>,
    _phantom_v: PhantomData<V>,
}

impl<K, V> RedisCache<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    #[allow(clippy::new_ret_no_self)]
    /// Initialize a `RedisCacheBuilder`
    pub fn new<S: AsRef<str>>(prefix: S, seconds: u64) -> RedisCacheBuilder<K, V> {
        RedisCacheBuilder::new(prefix, seconds)
    }

    fn generate_key(&self, key: &K) -> String {
        format!("{}{}", self.prefix, key)
    }

    /// Return the redis connection string used
    pub fn connection_string(&self) -> String {
        self.connection_string.clone()
    }
}

#[derive(Error, Debug)]
pub enum RedisCacheError {
    #[error("redis error")]
    RedisCacheError(#[from] redis::RedisError),
    #[error("redis pool error")]
    PoolError(#[from] r2d2::Error),
    #[error("Error deserializing cached value {cached_value:?}: {error:?}")]
    CacheDeserializationError {
        cached_value: String,
        error: serde_json::Error,
    },
    #[error("Error serializing cached value: {error:?}")]
    CacheSerializationError { error: serde_json::Error },
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedRedisValue<V> {
    value: V,
}

impl<'de, K, V> IOCached<K, V> for RedisCache<K, V>
where
    K: Display,
    V: Serialize + DeserializeOwned,
{
    type Error = RedisCacheError;

    fn cache_get(&self, key: &K) -> Result<Option<V>, RedisCacheError> {
        let mut conn = self.pool.get()?;
        let mut pipe = redis::pipe();
        let key = self.generate_key(key);

        pipe.get(key.clone());
        if self.refresh {
            pipe.expire(key, self.seconds as usize).ignore();
        }
        // ugh: https://github.com/mitsuhiko/redis-rs/pull/388#issuecomment-910919137
        let res: (Option<String>,) = pipe.query(&mut *conn)?;
        match res.0 {
            None => Ok(None),
            Some(s) => {
                let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                    RedisCacheError::CacheDeserializationError {
                        cached_value: s,
                        error: e,
                    }
                })?;
                Ok(Some(v.value))
            }
        }
    }

    fn cache_set(&self, key: K, val: V) -> Result<Option<V>, RedisCacheError> {
        let mut conn = self.pool.get()?;
        let mut pipe = redis::pipe();
        let key = self.generate_key(&key);

        let val = CachedRedisValue { value: val };
        pipe.get(key.clone());
        pipe.set_ex::<String, String>(
            key,
            serde_json::to_string(&val)
                .map_err(|e| RedisCacheError::CacheSerializationError { error: e })?,
            self.seconds as usize,
        )
        .ignore();

        let res: (Option<String>,) = pipe.query(&mut *conn)?;
        match res.0 {
            None => Ok(None),
            Some(s) => {
                let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                    RedisCacheError::CacheDeserializationError {
                        cached_value: s,
                        error: e,
                    }
                })?;
                Ok(Some(v.value))
            }
        }
    }

    fn cache_remove(&self, key: &K) -> Result<Option<V>, RedisCacheError> {
        let mut conn = self.pool.get()?;
        let mut pipe = redis::pipe();
        let key = self.generate_key(key);

        pipe.get(key.clone());
        pipe.del::<String>(key).ignore();
        let res: (Option<String>,) = pipe.query(&mut *conn)?;
        match res.0 {
            None => Ok(None),
            Some(s) => {
                let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                    RedisCacheError::CacheDeserializationError {
                        cached_value: s,
                        error: e,
                    }
                })?;
                Ok(Some(v.value))
            }
        }
    }

    fn cache_lifespan(&self) -> Option<u64> {
        Some(self.seconds)
    }

    fn cache_set_lifespan(&mut self, seconds: u64) -> Option<u64> {
        let old = self.seconds;
        self.seconds = seconds;
        Some(old)
    }

    fn cache_set_refresh(&mut self, refresh: bool) -> bool {
        let old = self.refresh;
        self.refresh = refresh;
        old
    }
}

#[cfg(all(
    feature = "async",
    any(feature = "redis_async_std", feature = "redis_tokio")
))]
mod async_redis {
    use super::*;
    use {crate::IOCachedAsync, async_trait::async_trait};

    pub struct AsyncRedisCacheBuilder<K, V> {
        seconds: u64,
        refresh: bool,
        prefix: String,
        connection_string: Option<String>,
        _phantom_k: PhantomData<K>,
        _phantom_v: PhantomData<V>,
    }

    impl<K, V> AsyncRedisCacheBuilder<K, V>
    where
        K: Display,
        V: Serialize + DeserializeOwned,
    {
        /// Initialize a `RedisCacheBuilder`
        pub fn new<S: AsRef<str>>(prefix: S, seconds: u64) -> AsyncRedisCacheBuilder<K, V> {
            Self {
                seconds,
                refresh: false,
                prefix: generate_prefix(prefix.as_ref()),
                connection_string: None,
                _phantom_k: Default::default(),
                _phantom_v: Default::default(),
            }
        }

        /// Specify the cache TTL/lifespan in seconds
        pub fn set_lifespan(mut self, seconds: u64) -> Self {
            self.seconds = seconds;
            self
        }

        /// Specify whether cache hits refresh the TTL
        pub fn set_refresh(mut self, refresh: bool) -> Self {
            self.refresh = refresh;
            self
        }

        /// Set the prefix for the keys
        pub fn set_prefix<S: AsRef<str>>(mut self, prefix: S) -> Self {
            self.prefix = generate_prefix(prefix.as_ref());
            self
        }

        /// Set the connection string for redis
        pub fn set_connection_string(mut self, cs: &str) -> Self {
            self.connection_string = Some(cs.to_string());
            self
        }

        /// Return the current connection string or load from the env var: CACHED_REDIS_CONNECTION_STRING
        pub fn connection_string(&self) -> Result<String, RedisCacheBuildError> {
            match self.connection_string {
                Some(ref s) => Ok(s.to_string()),
                None => std::env::var(ENV_KEY).map_err(|e| {
                    RedisCacheBuildError::MissingConnectionString {
                        env_key: ENV_KEY.to_string(),
                        error: e,
                    }
                }),
            }
        }

        async fn create_multiplexed_connection(
            &self,
        ) -> Result<redis::aio::MultiplexedConnection, RedisCacheBuildError> {
            let s = self.connection_string()?;
            let client = redis::Client::open(s)?;
            let conn = client.get_multiplexed_async_connection().await?;
            Ok(conn)
        }

        pub async fn build(self) -> Result<AsyncRedisCache<K, V>, RedisCacheBuildError> {
            Ok(AsyncRedisCache {
                seconds: self.seconds,
                refresh: self.refresh,
                connection_string: self.connection_string()?,
                multiplexed_connection: self.create_multiplexed_connection().await?,
                prefix: self.prefix,
                _phantom_k: self._phantom_k,
                _phantom_v: self._phantom_v,
            })
        }
    }

    /// Cache store backed by redis
    ///
    /// Values have a ttl applied and enforced by redis.
    pub struct AsyncRedisCache<K, V> {
        pub(super) seconds: u64,
        pub(super) refresh: bool,
        pub(super) prefix: String,
        connection_string: String,
        multiplexed_connection: redis::aio::MultiplexedConnection,
        _phantom_k: PhantomData<K>,
        _phantom_v: PhantomData<V>,
    }

    impl<K, V> AsyncRedisCache<K, V>
    where
        K: Display + Send + Sync,
        V: Serialize + DeserializeOwned + Send + Sync,
    {
        #[allow(clippy::new_ret_no_self)]
        /// Initialize an `AsyncRedisCacheBuilder`
        pub fn new<S: AsRef<str>>(prefix: S, seconds: u64) -> AsyncRedisCacheBuilder<K, V> {
            AsyncRedisCacheBuilder::new(prefix, seconds)
        }

        fn generate_key(&self, key: &K) -> String {
            format!("{}{}", self.prefix, key)
        }

        /// Return the redis connection string used
        pub fn connection_string(&self) -> String {
            self.connection_string.clone()
        }
    }

    #[async_trait]
    impl<'de, K, V> IOCachedAsync<K, V> for AsyncRedisCache<K, V>
    where
        K: Display + Send + Sync,
        V: Serialize + DeserializeOwned + Send + Sync,
    {
        type Error = RedisCacheError;

        /// Get a cached value
        async fn cache_get(&self, key: &K) -> Result<Option<V>, Self::Error> {
            let mut conn = self.multiplexed_connection.clone();
            let mut pipe = redis::pipe();
            let key = self.generate_key(key);

            pipe.get(key.clone());
            if self.refresh {
                pipe.expire(key, self.seconds as usize).ignore();
            }
            let res: (Option<String>,) = pipe.query_async(&mut conn).await?;
            match res.0 {
                None => Ok(None),
                Some(s) => {
                    let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                        RedisCacheError::CacheDeserializationError {
                            cached_value: s,
                            error: e,
                        }
                    })?;
                    Ok(Some(v.value))
                }
            }
        }

        /// Set a cached value
        async fn cache_set(&self, key: K, val: V) -> Result<Option<V>, Self::Error> {
            let mut conn = self.multiplexed_connection.clone();
            let mut pipe = redis::pipe();
            let key = self.generate_key(&key);

            let val = CachedRedisValue { value: val };
            pipe.get(key.clone());
            pipe.set_ex::<String, String>(
                key,
                serde_json::to_string(&val)
                    .map_err(|e| RedisCacheError::CacheSerializationError { error: e })?,
                self.seconds as usize,
            )
            .ignore();

            let res: (Option<String>,) = pipe.query_async(&mut conn).await?;
            match res.0 {
                None => Ok(None),
                Some(s) => {
                    let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                        RedisCacheError::CacheDeserializationError {
                            cached_value: s,
                            error: e,
                        }
                    })?;
                    Ok(Some(v.value))
                }
            }
        }

        /// Remove a cached value
        async fn cache_remove(&self, key: &K) -> Result<Option<V>, Self::Error> {
            let mut conn = self.multiplexed_connection.clone();
            let mut pipe = redis::pipe();
            let key = self.generate_key(key);

            pipe.get(key.clone());
            pipe.del::<String>(key).ignore();
            let res: (Option<String>,) = pipe.query_async(&mut conn).await?;
            match res.0 {
                None => Ok(None),
                Some(s) => {
                    let v: CachedRedisValue<V> = serde_json::from_str(&s).map_err(|e| {
                        RedisCacheError::CacheDeserializationError {
                            cached_value: s,
                            error: e,
                        }
                    })?;
                    Ok(Some(v.value))
                }
            }
        }

        /// Set the flag to control whether cache hits refresh the ttl of cached values, returns the old flag value
        fn cache_set_refresh(&mut self, refresh: bool) -> bool {
            let old = self.refresh;
            self.refresh = refresh;
            old
        }

        /// Return the lifespan of cached values (time to eviction)
        fn cache_lifespan(&self) -> Option<u64> {
            Some(self.seconds)
        }

        /// Set the lifespan of cached values, returns the old value
        fn cache_set_lifespan(&mut self, seconds: u64) -> Option<u64> {
            let old = self.seconds;
            self.seconds = seconds;
            Some(old)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::thread::sleep;
        use std::time::Duration;

        fn now_millis() -> u128 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        }

        #[async_std::test]
        async fn test_async_redis_cache() {
            let mut c: AsyncRedisCache<u32, u32> =
                AsyncRedisCache::new(format!("{}:async-redis-cache-test", now_millis()), 2)
                    .build()
                    .await
                    .unwrap();

            assert!(c.cache_get(&1).await.unwrap().is_none());

            assert!(c.cache_set(1, 100).await.unwrap().is_none());
            assert!(c.cache_get(&1).await.unwrap().is_some());

            sleep(Duration::new(2, 500000));
            assert!(c.cache_get(&1).await.unwrap().is_none());

            let old = c.cache_set_lifespan(1).unwrap();
            assert_eq!(2, old);
            assert!(c.cache_set(1, 100).await.unwrap().is_none());
            assert!(c.cache_get(&1).await.unwrap().is_some());

            sleep(Duration::new(1, 600000));
            assert!(c.cache_get(&1).await.unwrap().is_none());

            c.cache_set_lifespan(10).unwrap();
            assert!(c.cache_set(1, 100).await.unwrap().is_none());
            assert!(c.cache_set(2, 100).await.unwrap().is_none());
            assert_eq!(c.cache_get(&1).await.unwrap().unwrap(), 100);
            assert_eq!(c.cache_get(&1).await.unwrap().unwrap(), 100);
        }
    }
}

#[cfg(all(
    feature = "async",
    any(feature = "redis_async_std", feature = "redis_tokio")
))]
pub use async_redis::AsyncRedisCache;

#[cfg(test)]
/// Cache store tests
mod tests {
    use std::thread::sleep;
    use std::time::Duration;

    use super::*;

    fn now_millis() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    }

    #[test]
    fn redis_cache() {
        let mut c: RedisCache<u32, u32> =
            RedisCache::new(format!("{}:redis-cache-test", now_millis()), 2)
                .build()
                .unwrap();

        assert!(c.cache_get(&1).unwrap().is_none());

        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_get(&1).unwrap().is_some());

        sleep(Duration::new(2, 500000));
        assert!(c.cache_get(&1).unwrap().is_none());

        let old = c.cache_set_lifespan(1).unwrap();
        assert_eq!(2, old);
        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_get(&1).unwrap().is_some());

        sleep(Duration::new(1, 600000));
        assert!(c.cache_get(&1).unwrap().is_none());

        c.cache_set_lifespan(10).unwrap();
        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_set(2, 100).unwrap().is_none());
        assert_eq!(c.cache_get(&1).unwrap().unwrap(), 100);
        assert_eq!(c.cache_get(&1).unwrap().unwrap(), 100);
    }

    #[test]
    fn remove() {
        let c: RedisCache<u32, u32> =
            RedisCache::new(format!("{}:redis-cache-test-remove", now_millis()), 3600)
                .build()
                .unwrap();

        assert!(c.cache_set(1, 100).unwrap().is_none());
        assert!(c.cache_set(2, 200).unwrap().is_none());
        assert!(c.cache_set(3, 300).unwrap().is_none());

        assert_eq!(100, c.cache_remove(&1).unwrap().unwrap());
    }
}
