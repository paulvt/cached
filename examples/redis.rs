/*
Start a redis docker image if you don't already have it running locally:
    docker run --rm --name cached-redis-example -p 6379:6379 -d redis
Set the required env variable and run this example and run with required features:
    CACHED_REDIS_CONNECTION_STRING=redis://127.0.0.1:6379 cargo run --example redis --features "async redis_store redis_tokio"
Cleanup the redis docker container:
    docker rm -f cached-redis-example
 */

use cached::proc_macro::io_cached;
use cached::AsyncRedisCache;
use std::io;
use std::io::Write;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Clone)]
enum ExampleError {
    #[error("error with redis cache `{0}`")]
    RedisError(String),
}

// When the macro constructs your RedisCache instance, the connection string
// will be pulled from the env var: `CACHED_REDIS_CONNECTION_STRING`;
#[io_cached(
    redis = true,
    time = 30,
    cache_prefix_block = r##"{ "cache-redis-example-1" }"##,
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##
)]
fn cached_sleep_secs(secs: u64) -> Result<(), ExampleError> {
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}

// If not `cache_prefix_block` is specified, then the function name
// is used to create a prefix for cache keys used by this function
#[io_cached(
    redis = true,
    time = 30,
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##
)]
fn cached_sleep_secs_example_2(secs: u64) -> Result<(), ExampleError> {
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}

#[io_cached(
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##,
    type = "cached::AsyncRedisCache<u64, String>",
    create = r##" {
        AsyncRedisCache::new("cache_redis_example_cached_sleep_secs", 1)
            .set_refresh(true)
            .build()
            .await
            .expect("error building example redis cache")
    } "##
)]
async fn async_cached_sleep_secs(secs: u64) -> Result<String, ExampleError> {
    std::thread::sleep(Duration::from_secs(secs));
    Ok(secs.to_string())
}

struct Config {
    conn_str: String,
}
impl Config {
    fn load() -> Self {
        Self {
            conn_str: std::env::var("CACHED_REDIS_CONNECTION_STRING").unwrap(),
        }
    }
}
lazy_static::lazy_static! {
    static ref CONFIG: Config = Config::load();
}

#[io_cached(
    map_error = r##"|e| ExampleError::RedisError(format!("{:?}", e))"##,
    type = "cached::AsyncRedisCache<u64, String>",
    create = r##" {
        AsyncRedisCache::new("cache_redis_example_cached_sleep_secs_config", 1)
            .set_refresh(true)
            .set_connection_string(&CONFIG.conn_str)
            .build()
            .await
            .expect("error building example redis cache")
    } "##
)]
async fn async_cached_sleep_secs_config(secs: u64) -> Result<String, ExampleError> {
    std::thread::sleep(Duration::from_secs(secs));
    Ok(secs.to_string())
}

#[tokio::main]
async fn main() {
    print!("1. first sync call with a 2 seconds sleep...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).unwrap();
    println!("done");
    print!("second sync call with a 2 seconds sleep (it should be fast)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs(2).unwrap();
    println!("done");

    print!("2. first sync call with a 2 seconds sleep...");
    io::stdout().flush().unwrap();
    cached_sleep_secs_example_2(2).unwrap();
    println!("done");
    print!("second sync call with a 2 seconds sleep (it should be fast)...");
    io::stdout().flush().unwrap();
    cached_sleep_secs_example_2(2).unwrap();
    println!("done");

    print!("3. first async call with a 2 seconds sleep...");
    io::stdout().flush().unwrap();
    async_cached_sleep_secs(2).await.unwrap();
    println!("done");
    print!("second async call with a 2 seconds sleep (it should be fast)...");
    io::stdout().flush().unwrap();
    async_cached_sleep_secs(2).await.unwrap();
    println!("done");

    async_cached_sleep_secs_config_prime_cache(2).await.unwrap();
    print!("4. first primed async call with a 2 seconds sleep (should be fast)...");
    io::stdout().flush().unwrap();
    async_cached_sleep_secs_config(2).await.unwrap();
    println!("done");
    print!("second async call with a 2 seconds sleep (it should be fast)...");
    io::stdout().flush().unwrap();
    async_cached_sleep_secs_config(2).await.unwrap();
    println!("done");
}
