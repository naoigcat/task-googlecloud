use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::form_urlencoded::Serializer;

use crate::atomic_rename::DirectoryIdentity;
use crate::cloud::Cloud;
use crate::error::AppError;
use crate::upload_source;

/// Uploads are confined to this directory, so file discovery and the confined
/// open must agree on where it is.
pub(crate) const UPLOAD_ROOT: &str = "uploads";
/// Cloud Storage rejects object names longer than this, so reject them before
/// any request is sent and the transaction is half applied.
pub(crate) const MAX_OBJECT_NAME_BYTES: usize = 1024;
const API_BASE: &str = "https://storage.googleapis.com/storage/v1";
const UPLOAD_BASE: &str = "https://storage.googleapis.com/upload/storage/v1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// A generation-guarded marker serializes mutations through this client, so a
/// move cannot delete its source while another compliant writer replaces its
/// destination.
const BUCKET_LOCK_OBJECT: &str = ".task-googlecloud-lock";
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'*')
    .add(b'/')
    .add(b':')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ObjectPath {
    pub bucket: String,
    pub object: String,
}

impl ObjectPath {
    pub fn parse(value: &str) -> Result<Self, AppError> {
        let Some(value) = value.strip_prefix("gs://") else {
            return Err(AppError::InvalidStorageUri(value.to_string()));
        };
        let Some((bucket, object)) = value.split_once('/') else {
            return Err(AppError::InvalidStorageUri(format!("gs://{value}")));
        };
        if bucket.is_empty() || object.is_empty() {
            return Err(AppError::InvalidStorageUri(format!("gs://{value}")));
        }
        Ok(Self {
            bucket: bucket.to_string(),
            object: object.to_string(),
        })
    }

    pub fn uri(&self) -> String {
        format!("gs://{}/{}", self.bucket, self.object)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectMetadata {
    pub generation: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectState {
    Present,
    Missing,
}

#[derive(Debug)]
struct BucketLock {
    object: ObjectPath,
    generation: String,
}

pub trait StorageClient {
    fn list_objects(&self, bucket: &str) -> Result<Vec<String>, AppError>;
    fn set_upload_root_identity(
        &self,
        _identity: Option<DirectoryIdentity>,
    ) -> Result<(), AppError> {
        Ok(())
    }
    fn upload_file(&self, source: &Path, target: &ObjectPath) -> Result<String, AppError>;
    fn move_object(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        expected_source_generation: Option<&str>,
    ) -> Result<String, AppError>;
    fn rollback_object(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        target_generation: &str,
    ) -> Result<String, AppError>;
    fn cleanup_object(&self, target: &ObjectPath, target_generation: &str) -> Result<(), AppError>;
    fn confirm_move_after_failure(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        operation: &AppError,
    ) -> Result<(), AppError>;
    fn confirm_write_after_failure(
        &self,
        target: &ObjectPath,
        operation: &AppError,
    ) -> Result<(), AppError>;
}

pub struct StorageApi {
    cloud: Cloud,
    client: Client,
    upload_client: Client,
    token_override: Option<String>,
    api_base: String,
    upload_base: String,
    upload_root: Option<PathBuf>,
    upload_root_identity: Mutex<Option<DirectoryIdentity>>,
}

impl StorageApi {
    pub fn new(cloud: Cloud) -> Self {
        let mut storage = Self::with_endpoints(cloud, API_BASE, UPLOAD_BASE, None);
        storage.upload_root = Some(PathBuf::from(UPLOAD_ROOT));
        storage
    }

    pub fn with_endpoints(
        cloud: Cloud,
        api_base: impl Into<String>,
        upload_base: impl Into<String>,
        token: Option<String>,
    ) -> Self {
        Self::with_endpoint_options(cloud, api_base, upload_base, token, REQUEST_TIMEOUT)
    }

    fn with_endpoint_options(
        cloud: Cloud,
        api_base: impl Into<String>,
        upload_base: impl Into<String>,
        token: Option<String>,
        request_timeout: Duration,
    ) -> Self {
        Self::with_endpoint_options_and_upload_timeout(
            cloud,
            api_base,
            upload_base,
            token,
            request_timeout,
            UPLOAD_TIMEOUT,
        )
    }

    fn with_endpoint_options_and_upload_timeout(
        cloud: Cloud,
        api_base: impl Into<String>,
        upload_base: impl Into<String>,
        token: Option<String>,
        request_timeout: Duration,
        upload_timeout: Duration,
    ) -> Self {
        Self {
            cloud,
            client: Self::build_client(Some(request_timeout)),
            upload_client: Self::build_client(Some(upload_timeout)),
            token_override: token,
            api_base: api_base.into(),
            upload_base: upload_base.into(),
            upload_root: None,
            upload_root_identity: Mutex::new(None),
        }
    }

    fn build_client(total_timeout: Option<Duration>) -> Client {
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

    fn send(&self, request: RequestBuilder) -> Result<Response, AppError> {
        let token = self.token()?;
        Ok(request.bearer_auth(token).send()?)
    }

    fn send_body(&self, request: RequestBuilder) -> Result<Vec<u8>, AppError> {
        Self::response_body(self.send(request)?)
    }

    fn send_json<T>(&self, request: RequestBuilder, description: &str) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        let body = self.send_body(request)?;
        serde_json::from_slice(&body)
            .map_err(|error| AppError::Message(format!("{description}: {error}")))
    }

    fn response_body(response: Response) -> Result<Vec<u8>, AppError> {
        let status = response.status();
        let body = response.bytes()?.to_vec();
        if status.is_success() {
            return Ok(body);
        }

        Err(AppError::Storage {
            status: status.as_u16(),
            message: response_message(&body),
        })
    }

    fn acquire_bucket_lock(&self, bucket: &str) -> Result<BucketLock, AppError> {
        let object = ObjectPath {
            bucket: bucket.to_string(),
            object: BUCKET_LOCK_OBJECT.to_string(),
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let url = with_query(
            format!("{}/b/{}/o", self.upload_base, encode(bucket)),
            [
                ("uploadType", "media"),
                ("name", BUCKET_LOCK_OBJECT),
                ("ifGenerationMatch", "0"),
            ],
        );
        let metadata: Result<MetadataResponse, AppError> = self.send_json(
            self.client
                .post(url)
                .header(CONTENT_TYPE, "application/octet-stream")
                .header(CONTENT_LENGTH, token.len() as u64)
                .body(token.as_bytes().to_vec()),
            "Invalid Cloud Storage bucket lock response",
        );
        let metadata = match metadata {
            Ok(metadata) => metadata,
            Err(error) if error.status() != Some(412) && error.reached_storage() => {
                match self.recover_unacknowledged_bucket_lock(&object, &token) {
                    Ok(()) => return Err(error),
                    Err(recovery) => return Err(AppError::rollback(error, vec![recovery])),
                }
            }
            Err(error) => return Err(error),
        };
        Ok(BucketLock {
            object,
            generation: metadata.generation,
        })
    }

    fn recover_unacknowledged_bucket_lock(
        &self,
        object: &ObjectPath,
        token: &str,
    ) -> Result<(), AppError> {
        let body = self.send_body(self.client.get(with_query(
            object_url(&self.api_base, object),
            [("alt", "media")],
        )))?;
        if body != token.as_bytes() {
            return Ok(());
        }
        let generation = self.object_metadata(object, None)?.generation;
        self.delete_object(object, &generation)
    }

    fn reject_bucket_lock_object(object: &ObjectPath) -> Result<(), AppError> {
        if object.object == BUCKET_LOCK_OBJECT {
            return Err(AppError::Message(format!(
                "The reserved bucket lock object cannot be modified: {}",
                object.uri()
            )));
        }
        Ok(())
    }

    fn release_bucket_locks(&self, locks: &[BucketLock]) -> Vec<AppError> {
        locks
            .iter()
            .rev()
            .filter_map(|lock| self.delete_object(&lock.object, &lock.generation).err())
            .collect()
    }

    fn with_bucket_locks<T, F>(&self, buckets: &[&str], operation: F) -> Result<T, AppError>
    where
        F: FnOnce() -> Result<T, AppError>,
    {
        let mut bucket_names = buckets.to_vec();
        bucket_names.sort_unstable();
        bucket_names.dedup();

        let mut locks = Vec::with_capacity(bucket_names.len());
        for bucket in bucket_names {
            match self.acquire_bucket_lock(bucket) {
                Ok(lock) => locks.push(lock),
                Err(error) => {
                    return Err(AppError::rollback(error, self.release_bucket_locks(&locks)));
                }
            }
        }

        let result = operation();
        let release_errors = self.release_bucket_locks(&locks);
        match result {
            Ok(value) if release_errors.is_empty() => Ok(value),
            Ok(_) => Err(AppError::Recovery {
                paths: locks
                    .iter()
                    .map(|lock| lock.object.uri())
                    .collect::<Vec<_>>()
                    .join(", "),
                operation: "release Cloud Storage bucket lock".to_string(),
                details: release_errors
                    .into_iter()
                    .map(|error| error.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            }),
            Err(error) => Err(AppError::rollback(error, release_errors)),
        }
    }

    fn object_metadata(
        &self,
        object: &ObjectPath,
        generation: Option<&str>,
    ) -> Result<ObjectMetadata, AppError> {
        let mut url = object_url(&self.api_base, object);
        if let Some(generation) = generation {
            url = with_query(url, [("generation", generation)]);
        }
        let metadata: MetadataResponse =
            self.send_json(self.client.get(url), "Invalid Cloud Storage metadata")?;
        Ok(ObjectMetadata {
            generation: metadata.generation,
        })
    }

    fn object_state(
        &self,
        object: &ObjectPath,
        generation: Option<&str>,
    ) -> Result<ObjectState, AppError> {
        match self.object_metadata(object, generation) {
            Ok(_) => Ok(ObjectState::Present),
            Err(error) if error.status() == Some(404) => Ok(ObjectState::Missing),
            Err(error) => Err(error),
        }
    }

    pub fn copy_object(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        source_generation: Option<&str>,
        destination_generation: Option<&str>,
    ) -> Result<String, AppError> {
        Self::reject_bucket_lock_object(source)?;
        Self::reject_bucket_lock_object(target)?;
        self.with_bucket_locks(&[source.bucket.as_str(), target.bucket.as_str()], || {
            self.copy_object_unlocked(source, target, source_generation, destination_generation)
        })
    }

    fn copy_object_unlocked(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        source_generation: Option<&str>,
        destination_generation: Option<&str>,
    ) -> Result<String, AppError> {
        let mut query = Serializer::new(String::new());
        if let Some(generation) = source_generation {
            query.append_pair("sourceGeneration", generation);
            query.append_pair("ifSourceGenerationMatch", generation);
        }
        if let Some(generation) = destination_generation {
            query.append_pair("ifGenerationMatch", generation);
        }
        let query = query.finish();
        let base_url = rewrite_url(&self.api_base, source, target);
        let mut rewrite_token: Option<String> = None;
        let mut request_sent = false;

        loop {
            let mut url = base_url.clone();
            let mut query_with_token = query.clone();
            if let Some(token) = &rewrite_token {
                if !query_with_token.is_empty() {
                    query_with_token.push('&');
                }
                query_with_token.push_str(&query_pair("rewriteToken", token));
            }
            if !query_with_token.is_empty() {
                url.push('?');
                url.push_str(&query_with_token);
            }

            let rewrite: RewriteResponse = self
                .send_json(
                    self.client.post(url),
                    "Invalid Cloud Storage rewrite response",
                )
                .map_err(|error| {
                    if request_sent {
                        error.mark_reached_storage()
                    } else {
                        error
                    }
                })?;
            request_sent = true;
            if rewrite.done {
                let resource = rewrite.resource.ok_or_else(|| {
                    AppError::Message("Cloud Storage rewrite omitted its resource".to_string())
                })?;
                return Ok(resource.generation);
            }
            rewrite_token = Some(rewrite.rewrite_token.ok_or_else(|| {
                AppError::Message(
                    "Cloud Storage rewrite omitted its continuation token".to_string(),
                )
            })?);
        }
    }

    fn delete_object(&self, object: &ObjectPath, generation: &str) -> Result<(), AppError> {
        let url = with_query(
            object_url(&self.api_base, object),
            [("generation", generation)],
        );
        self.send_body(self.client.delete(url)).map(|_| ())
    }

    fn confirm_object_generation(
        &self,
        object: &ObjectPath,
        expected_generation: &str,
        operation: &str,
    ) -> Result<(), AppError> {
        let details = self.object_details(object);
        if matches!(
            &details,
            Ok((ObjectState::Present, Some(generation)))
                if generation == expected_generation
        ) {
            return Ok(());
        }

        Err(AppError::Recovery {
            paths: format!("{:?}", object.uri()),
            operation: operation.to_string(),
            details: format!(
                "Expected generation {expected_generation}; {}",
                state_details(object, details)
            ),
        })
    }
}

impl StorageClient for StorageApi {
    fn list_objects(&self, bucket: &str) -> Result<Vec<String>, AppError> {
        let mut page_token: Option<String> = None;
        let mut objects = Vec::new();
        loop {
            let mut query = vec![("maxResults", "1000")];
            if let Some(page_token) = &page_token {
                query.push(("pageToken", page_token.as_str()));
            }
            let url = with_query(format!("{}/b/{}/o", self.api_base, encode(bucket)), query);
            let listing: ListResponse =
                self.send_json(self.client.get(url), "Invalid Cloud Storage list response")?;
            objects.extend(
                listing
                    .items
                    .into_iter()
                    .filter(|item| item.name != BUCKET_LOCK_OBJECT)
                    .map(|item| format!("gs://{bucket}/{}", item.name)),
            );
            page_token = listing.next_page_token;
            if page_token.is_none() {
                return Ok(objects);
            }
        }
    }

    fn set_upload_root_identity(
        &self,
        identity: Option<DirectoryIdentity>,
    ) -> Result<(), AppError> {
        *self.upload_root_identity.lock().map_err(|_| {
            AppError::Message("Upload root identity lock is poisoned".to_string())
        })? = identity;
        Ok(())
    }

    fn upload_file(&self, source: &Path, target: &ObjectPath) -> Result<String, AppError> {
        Self::reject_bucket_lock_object(target)?;
        let expected_root = *self
            .upload_root_identity
            .lock()
            .map_err(|_| AppError::Message("Upload root identity lock is poisoned".to_string()))?;
        let file = upload_source::open(self.upload_root.as_deref(), source, expected_root)?;
        let size = file.metadata().map_err(AppError::UploadSource)?.len();
        self.with_bucket_locks(&[target.bucket.as_str()], || {
            let url = with_query(
                format!("{}/b/{}/o", self.upload_base, encode(&target.bucket)),
                [
                    ("uploadType", "media"),
                    ("name", target.object.as_str()),
                    ("ifGenerationMatch", "0"),
                ],
            );
            let request = self
                .upload_client
                .post(url)
                .header(CONTENT_TYPE, "application/octet-stream")
                .header(CONTENT_LENGTH, size)
                .body(file);
            let metadata: MetadataResponse =
                self.send_json(request, "Invalid Cloud Storage upload response")?;
            Ok(metadata.generation)
        })
    }

    fn move_object(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        expected_source_generation: Option<&str>,
    ) -> Result<String, AppError> {
        Self::reject_bucket_lock_object(source)?;
        Self::reject_bucket_lock_object(target)?;
        self.with_bucket_locks(&[source.bucket.as_str(), target.bucket.as_str()], || {
            self.move_object_unlocked(source, target, expected_source_generation)
        })
    }

    fn rollback_object(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        target_generation: &str,
    ) -> Result<String, AppError> {
        Self::reject_bucket_lock_object(source)?;
        Self::reject_bucket_lock_object(target)?;
        self.with_bucket_locks(&[source.bucket.as_str(), target.bucket.as_str()], || {
            self.rollback_object_unlocked(source, target, target_generation)
        })
    }

    fn cleanup_object(&self, target: &ObjectPath, target_generation: &str) -> Result<(), AppError> {
        Self::reject_bucket_lock_object(target)?;
        self.with_bucket_locks(&[target.bucket.as_str()], || {
            self.cleanup_object_unlocked(target, target_generation)
        })
    }

    fn confirm_move_after_failure(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        operation: &AppError,
    ) -> Result<(), AppError> {
        // Nothing was sent, so neither object can have moved.
        if !operation.reached_storage() {
            return Ok(());
        }

        let source_details = self.object_details(source);
        let target_details = self.object_details(target);
        let no_change = matches!(source_details, Ok((ObjectState::Present, _)))
            && matches!(target_details, Ok((ObjectState::Missing, _)));
        if no_change && !operation.may_have_sent_storage_request() {
            return Ok(());
        }
        Err(AppError::Recovery {
            paths: format!("{:?} and {:?}", source.uri(), target.uri()),
            operation: operation.to_string(),
            details: format!(
                "{}; {}",
                state_details(source, source_details),
                state_details(target, target_details)
            ),
        })
    }

    fn confirm_write_after_failure(
        &self,
        target: &ObjectPath,
        operation: &AppError,
    ) -> Result<(), AppError> {
        // Nothing was sent, so the target cannot exist and asking would only
        // turn a local failure into a spurious manual recovery.
        if !operation.reached_storage() {
            return Ok(());
        }

        let details = self.object_details(target);
        if matches!(details, Ok((ObjectState::Missing, _)))
            && !operation.may_have_sent_storage_request()
        {
            return Ok(());
        }
        Err(AppError::Recovery {
            paths: format!("{:?}", target.uri()),
            operation: operation.to_string(),
            details: state_details(target, details),
        })
    }
}

impl StorageApi {
    fn move_object_unlocked(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        expected_source_generation: Option<&str>,
    ) -> Result<String, AppError> {
        let source_generation = match expected_source_generation {
            Some(generation) => generation.to_string(),
            None => self.object_metadata(source, None)?.generation,
        };
        let target_generation =
            self.copy_object_unlocked(source, target, Some(&source_generation), Some("0"))?;
        self.confirm_object_generation(target, &target_generation, "move object")?;
        self.delete_object(source, &source_generation)
            .map_err(AppError::mark_reached_storage)?;
        let source_state = self
            .object_state(source, None)
            .map_err(AppError::mark_reached_storage)?;
        self.confirm_object_generation(target, &target_generation, "move object")?;
        if source_state != ObjectState::Missing {
            return Err(AppError::Message(format!(
                "Source object remains after moving {}",
                source.uri()
            )));
        }
        Ok(target_generation)
    }

    fn rollback_object_unlocked(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        target_generation: &str,
    ) -> Result<String, AppError> {
        if self.object_state(source, None)? != ObjectState::Missing
            || self.object_state(target, Some(target_generation))? != ObjectState::Present
        {
            return Err(AppError::Message(format!(
                "Rollback ownership check failed for {} and {}",
                source.uri(),
                target.uri()
            )));
        }
        let source_generation =
            self.copy_object_unlocked(target, source, Some(target_generation), Some("0"))?;
        self.confirm_object_generation(source, &source_generation, "rollback object")?;
        self.delete_object(target, target_generation)
            .map_err(AppError::mark_reached_storage)?;
        let target_state = self
            .object_state(target, None)
            .map_err(AppError::mark_reached_storage)?;
        self.confirm_object_generation(source, &source_generation, "rollback object")?;
        if target_state != ObjectState::Missing {
            return Err(AppError::Message(format!(
                "Rollback target remains: {}",
                target.uri()
            )));
        }
        Ok(source_generation)
    }

    fn cleanup_object_unlocked(
        &self,
        target: &ObjectPath,
        target_generation: &str,
    ) -> Result<(), AppError> {
        match self.object_state(target, Some(target_generation))? {
            ObjectState::Present => self
                .delete_object(target, target_generation)
                .map_err(AppError::mark_reached_storage)?,
            ObjectState::Missing => {}
        }
        if self
            .object_state(target, None)
            .map_err(AppError::mark_reached_storage)?
            != ObjectState::Missing
        {
            return Err(AppError::Message(format!(
                "Cleanup target remains: {}",
                target.uri()
            )));
        }
        Ok(())
    }

    fn object_details(
        &self,
        object: &ObjectPath,
    ) -> Result<(ObjectState, Option<String>), AppError> {
        match self.object_metadata(object, None) {
            Ok(metadata) => Ok((ObjectState::Present, Some(metadata.generation))),
            Err(error) if error.status() == Some(404) => Ok((ObjectState::Missing, None)),
            Err(error) => Err(error),
        }
    }
}

/// A failed lookup is reported rather than propagated so that the caller still
/// learns which objects need manual recovery.
fn state_details(
    object: &ObjectPath,
    details: Result<(ObjectState, Option<String>), AppError>,
) -> String {
    match details {
        Ok((ObjectState::Missing, _)) => format!("{} is missing", object.uri()),
        Ok((ObjectState::Present, Some(generation))) => {
            format!("{}: generation {generation}", object.uri())
        }
        Ok((ObjectState::Present, None)) => format!("{}: generation unknown", object.uri()),
        Err(error) => format!("{}: state unknown ({error})", object.uri()),
    }
}

#[derive(Debug, Deserialize)]
struct MetadataResponse {
    generation: String,
}

#[derive(Debug, Deserialize)]
struct ListResponse {
    #[serde(default)]
    items: Vec<ListItem>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListItem {
    name: String,
}

#[derive(Debug, Deserialize)]
struct RewriteResponse {
    done: bool,
    #[serde(rename = "rewriteToken")]
    rewrite_token: Option<String>,
    resource: Option<MetadataResponse>,
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

fn encode(value: &str) -> String {
    utf8_percent_encode(value, PATH_SEGMENT).to_string()
}

fn object_url(base: &str, object: &ObjectPath) -> String {
    format!(
        "{base}/b/{}/o/{}",
        encode(&object.bucket),
        encode(&object.object)
    )
}

fn rewrite_url(base: &str, source: &ObjectPath, target: &ObjectPath) -> String {
    format!(
        "{base}/b/{}/o/{}/rewriteTo/b/{}/o/{}",
        encode(&source.bucket),
        encode(&source.object),
        encode(&target.bucket),
        encode(&target.object),
    )
}

fn query_pair(name: &str, value: &str) -> String {
    let mut serializer = Serializer::new(String::new());
    serializer.append_pair(name, value);
    serializer.finish()
}

fn with_query<I, K, V>(base: String, pairs: I) -> String
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut serializer = Serializer::new(String::new());
    for (key, value) in pairs {
        serializer.append_pair(key.as_ref(), value.as_ref());
    }
    format!("{base}?{}", serializer.finish())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;

    use tempfile::NamedTempFile;

    use super::{Cloud, Duration, ObjectPath, StorageApi, StorageClient};

    fn read_headers(stream: &mut TcpStream) {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut buffer).unwrap();
            request.push(buffer[0]);
        }
    }

    fn read_request(stream: &mut TcpStream) -> String {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut buffer).unwrap();
            request.push(buffer[0]);
        }
        let content_length = String::from_utf8_lossy(&request)
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        let mut body = vec![0; content_length];
        stream.read_exact(&mut body).unwrap();
        request.extend(body);
        String::from_utf8(request).unwrap()
    }

    fn write_json(stream: &mut TcpStream, body: &str) {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
    }

    #[test]
    fn uploads_files_with_a_longer_timeout_than_api_requests() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base = format!("http://{}/storage/v1", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut lock_stream, _) = listener.accept().unwrap();
            read_headers(&mut lock_stream);
            write_json(&mut lock_stream, r#"{"generation":"lock"}"#);

            let (mut stream, _) = listener.accept().unwrap();
            read_headers(&mut stream);
            thread::sleep(Duration::from_millis(100));
            write_json(&mut stream, r#"{"generation":"456"}"#);

            let (mut release_stream, _) = listener.accept().unwrap();
            read_headers(&mut release_stream);
            write_json(&mut release_stream, "{}");
        });
        let source = NamedTempFile::new().unwrap();
        std::fs::write(source.path(), b"contents").unwrap();
        let target = ObjectPath::parse("gs://bucket/target").unwrap();
        let storage = StorageApi::with_endpoint_options(
            Cloud::new(),
            base.clone(),
            base,
            Some("token".to_string()),
            Duration::from_millis(10),
        );

        assert_eq!(storage.upload_file(source.path(), &target).unwrap(), "456");
        server.join().unwrap();
    }

    #[test]
    fn times_out_uploads_that_stop_responding() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base = format!("http://{}/storage/v1", listener.local_addr().unwrap());
        let (headers_sent, headers_received) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut lock_stream, _) = listener.accept().unwrap();
            read_headers(&mut lock_stream);
            write_json(&mut lock_stream, r#"{"generation":"lock"}"#);

            let (mut stream, _) = listener.accept().unwrap();
            read_headers(&mut stream);
            headers_sent.send(()).unwrap();
            release_receiver.recv().unwrap();
            let (mut release_stream, _) = listener.accept().unwrap();
            read_headers(&mut release_stream);
            write_json(&mut release_stream, "{}");
        });
        let source = NamedTempFile::new().unwrap();
        std::fs::write(source.path(), b"contents").unwrap();
        let target = ObjectPath::parse("gs://bucket/target").unwrap();
        let storage = StorageApi::with_endpoint_options_and_upload_timeout(
            Cloud::new(),
            base.clone(),
            base,
            Some("token".to_string()),
            Duration::from_secs(30),
            Duration::from_millis(10),
        );
        let (result_sender, result_receiver) = mpsc::channel();
        let upload = thread::spawn(move || {
            result_sender
                .send(storage.upload_file(source.path(), &target))
                .unwrap();
        });

        headers_received
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        release_sender.send(()).unwrap();
        let result = result_receiver.recv_timeout(Duration::from_secs(1));
        upload.join().unwrap();
        server.join().unwrap();

        let error = result.unwrap().unwrap_err();

        assert!(matches!(error, super::AppError::Http(_)));
    }

    #[test]
    fn removes_a_bucket_lock_after_an_unacknowledged_response() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base = format!("http://{}/storage/v1", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut create_stream, _) = listener.accept().unwrap();
            let request = read_request(&mut create_stream);
            let token = request
                .split_once("\r\n\r\n")
                .map_or("", |(_, body)| body)
                .to_string();
            write_json(&mut create_stream, "not-json");

            let (mut media_stream, _) = listener.accept().unwrap();
            read_headers(&mut media_stream);
            write_json(&mut media_stream, &token);

            let (mut metadata_stream, _) = listener.accept().unwrap();
            read_headers(&mut metadata_stream);
            write_json(&mut metadata_stream, r#"{"generation":"lock"}"#);

            let (mut delete_stream, _) = listener.accept().unwrap();
            read_headers(&mut delete_stream);
            write_json(&mut delete_stream, "{}");
        });
        let storage =
            StorageApi::with_endpoints(Cloud::new(), base.clone(), base, Some("token".to_string()));
        let error = storage.acquire_bucket_lock("bucket").unwrap_err();

        server.join().unwrap();

        assert!(
            matches!(error, super::AppError::Message(message) if message.contains("bucket lock"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_upload_sources_through_parent_symlinks() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = outside.path().join("secret.txt");
        std::fs::write(&source, "secret").unwrap();
        let linked_bucket = root.join("linked-bucket");
        symlink(outside.path(), &linked_bucket).unwrap();
        let source_through_link = linked_bucket.join("secret.txt");
        let target = ObjectPath::parse("gs://bucket/target").unwrap();
        let mut storage = StorageApi::with_endpoint_options(
            Cloud::new(),
            "http://127.0.0.1:1/storage/v1",
            "http://127.0.0.1:1/storage/v1",
            Some("token".to_string()),
            Duration::from_millis(10),
        );
        storage.upload_root = Some(root);

        let error = storage
            .upload_file(&source_through_link, &target)
            .unwrap_err();

        assert!(matches!(error, super::AppError::UploadSource(_)), "{error}");
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn refuses_upload_sources_after_upload_root_is_replaced() {
        use crate::atomic_rename::directory_identity_from_metadata;

        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("uploads");
        std::fs::create_dir(&root).unwrap();
        let expected_root =
            directory_identity_from_metadata(&std::fs::symlink_metadata(&root).unwrap());
        let replacement = parent.path().join("replacement");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::remove_dir(&root).unwrap();
        std::fs::rename(&replacement, &root).unwrap();

        let source = root.join("bucket/file.txt");
        std::fs::create_dir(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "replacement").unwrap();
        let target = ObjectPath::parse("gs://bucket/target").unwrap();
        let mut storage = StorageApi::with_endpoint_options(
            Cloud::new(),
            "http://127.0.0.1:1/storage/v1",
            "http://127.0.0.1:1/storage/v1",
            Some("token".to_string()),
            Duration::from_millis(10),
        );
        storage.upload_root = Some(root);
        storage
            .set_upload_root_identity(Some(expected_root))
            .unwrap();

        let error = storage.upload_file(&source, &target).unwrap_err();

        assert!(matches!(error, super::AppError::UploadSource(_)), "{error}");
    }
}
