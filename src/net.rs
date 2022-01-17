use async_trait::async_trait;
use anyhow::Error;
use log::debug;
use rand::Rng;

#[async_trait]
pub trait Retry {
    async fn auto_retry(&self, attempts: u64) -> Result<reqwest::Response, Error>;
}

#[async_trait]
impl Retry for reqwest::RequestBuilder {
    async fn auto_retry(&self, max_attempts: u64) -> Result<reqwest::Response, Error> {
        /// The duration of the retry will be picked randomly within this interval,
        /// plus an exponential backoff.
        const BASE_INTERVAL_MS: std::ops::Range<u64> = 300..1000;

        let mut attempt = 1;
        loop {
            match self
                .try_clone()
                .expect("Cannot auto-retry non-clonable requests")
                .send()
                .await
            {
                Ok(response) => {
                    debug!("auto_retry success");
                    break Ok(response);
                }
                Err(err) => {
                    debug!("auto_retry error {:?} => {:?}", err, err.status());
                    // FIXME: Is this the right way to decide when to retry?
                    let should_retry = attempt < max_attempts
                        && (err.is_connect() || err.is_timeout() || err.is_request());

                    if should_retry {
                        let duration =
                            (attempt * attempt) * rand::thread_rng().gen_range(BASE_INTERVAL_MS);
                        attempt += 1;
                        debug!("auto_retry: sleeping {}ms", duration);
                        tokio::time::sleep(std::time::Duration::from_millis(duration)).await;
                    } else {
                        debug!("auto_retry: giving up!");
                        return Err(err.into());
                    }
                }
            }
        }
    }
}
