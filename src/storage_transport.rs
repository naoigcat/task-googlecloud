use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use reqwest::{Client, RequestBuilder, Response};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::InterruptFlag;
use crate::cloud::Cloud;
use crate::error::AppError;

pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Owns the HTTP runtime and authentication boundary so storage operations can
/// focus on Cloud Storage state transitions.
pub(crate) struct StorageTransport {
    cloud: Cloud,
    interrupt: Option<InterruptFlag>,
    runtime: Option<tokio::runtime::Runtime>,
    client: Client,
    upload_client: Client,
    token_override: Option<String>,
    api_base: String,
    upload_base: String,
}

impl StorageTransport {
    pub(crate) fn new(
        cloud: Cloud,
        api_base: impl Into<String>,
        upload_base: impl Into<String>,
        token: Option<String>,
        request_timeout: Duration,
        upload_timeout: Duration,
    ) -> Self {
        let interrupt = cloud.interrupt();
        Self {
            cloud,
            interrupt,
            runtime: Some(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("the HTTP runtime configuration is valid"),
            ),
            client: Self::build_client(request_timeout),
            upload_client: Self::build_client(upload_timeout),
            token_override: token,
            api_base: api_base.into(),
            upload_base: upload_base.into(),
        }
    }

    pub(crate) fn client(&self) -> &Client {
        &self.client
    }

    pub(crate) fn upload_client(&self) -> &Client {
        &self.upload_client
    }

    pub(crate) fn api_base(&self) -> &str {
        &self.api_base
    }

    pub(crate) fn upload_base(&self) -> &str {
        &self.upload_base
    }

    pub(crate) fn send_body(&self, request: RequestBuilder) -> Result<Vec<u8>, AppError> {
        let token = self.token()?;
        if self
            .interrupt
            .as_ref()
            .is_some_and(InterruptFlag::is_interrupted)
        {
            return Err(AppError::Interrupted);
        }
        let interrupt = self.interrupt.clone();
        // Record whether the HTTP future started before an interrupt won the
        // race; only requests that may have been sent require state recovery.
        let request_started = Arc::new(AtomicBool::new(false));
        let request_started_for_request = Arc::clone(&request_started);
        let request_started_for_select = Arc::clone(&request_started);
        let interrupt_for_request = interrupt.clone();
        // Dropping the async request closes the connection, so rollback never
        // races with a blocking request that was left running in the background.
        let result = self
            .runtime
            .as_ref()
            .expect("the HTTP runtime remains available while StorageTransport is alive")
            .block_on(async move {
                tokio::select! {
                    biased;
                    response = async move {
                        if interrupt_for_request
                            .as_ref()
                            .is_some_and(InterruptFlag::is_interrupted)
                        {
                            return Err(AppError::Interrupted);
                        }
                        request_started_for_request.store(true, Ordering::Relaxed);
                        let response = request.bearer_auth(token).send().await?;
                        Self::response_body(response).await
                    } => response,
                    _ = wait_for_interrupt(interrupt) => {
                        if request_started_for_select.load(Ordering::Relaxed) {
                            Err(AppError::InterruptedAfterRequest)
                        } else {
                            Err(AppError::Interrupted)
                        }
                    },
                }
            });
        if self
            .interrupt
            .as_ref()
            .is_some_and(InterruptFlag::is_interrupted)
        {
            return if request_started.load(Ordering::Relaxed) {
                Err(AppError::InterruptedAfterRequest)
            } else {
                result
            };
        }
        result
    }

    pub(crate) fn send_json<T>(
        &self,
        request: RequestBuilder,
        description: &str,
    ) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        let body = self.send_body(request)?;
        serde_json::from_slice(&body)
            .map_err(|error| AppError::StorageResponse(format!("{description}: {error}")))
    }

    pub(crate) fn clear_interrupt_for_rollback(&self) -> bool {
        self.interrupt
            .as_ref()
            .is_some_and(InterruptFlag::clear_for_rollback)
    }

    fn build_client(total_timeout: Duration) -> Client {
        Client::builder()
            .timeout(total_timeout)
            .connect_timeout(REQUEST_TIMEOUT)
            .build()
            .expect("the static HTTP client configuration is valid")
    }

    fn token(&self) -> Result<String, AppError> {
        self.token_override
            .clone()
            .map_or_else(|| self.cloud.access_token().map_err(AppError::token), Ok)
    }

    async fn response_body(response: Response) -> Result<Vec<u8>, AppError> {
        let status = response.status();
        let body = response.bytes().await?.to_vec();
        if status.is_success() {
            return Ok(body);
        }

        Err(AppError::Storage {
            status: status.as_u16(),
            message: response_message(&body),
        })
    }
}

impl Drop for StorageTransport {
    fn drop(&mut self) {
        // A stalled local read must not make process shutdown wait forever after SIGINT.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::ZERO);
        }
    }
}

async fn wait_for_interrupt(interrupt: Option<InterruptFlag>) {
    let Some(interrupt) = interrupt else {
        std::future::pending::<()>().await;
        return;
    };
    loop {
        if interrupt.is_interrupted() {
            return;
        }
        tokio::time::sleep(INTERRUPT_POLL_INTERVAL).await;
    }
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    error: Option<ErrorDetails>,
}

#[derive(Debug, Deserialize)]
struct ErrorDetails {
    message: Option<String>,
}

fn response_message(body: &[u8]) -> String {
    serde_json::from_slice::<ErrorResponse>(body)
        .ok()
        .and_then(|response| response.error)
        .and_then(|error| error.message)
        .unwrap_or_else(|| String::from_utf8_lossy(body).to_string())
}
