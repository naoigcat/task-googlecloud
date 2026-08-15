use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread::{self, ThreadId};
use std::time::Duration;

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::Body;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use serde::Deserialize;
use tokio_util::io::ReaderStream;
use url::form_urlencoded::Serializer;

#[cfg(test)]
pub(crate) use crate::InterruptFlag;
use crate::atomic_rename::DirectoryIdentity;
use crate::cloud::Cloud;
use crate::error::AppError;
pub use crate::object_path::ObjectPath;
use crate::storage_transport::{REQUEST_TIMEOUT, StorageTransport, UPLOAD_TIMEOUT};
use crate::upload_source::{self, UploadSourceIdentity};

/// Uploads are confined to this directory, so file discovery and the confined
/// open must agree on where it is.
pub(crate) const UPLOAD_ROOT: &str = "uploads";
const API_BASE: &str = "https://storage.googleapis.com/storage/v1";
const UPLOAD_BASE: &str = "https://storage.googleapis.com/upload/storage/v1";
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
    fn with_bucket_locks<T, F>(&self, _buckets: &[&str], operation: F) -> Result<T, AppError>
    where
        F: FnOnce() -> Result<T, AppError>,
    {
        operation()
    }
    fn set_upload_root_identity(
        &self,
        _identity: Option<DirectoryIdentity>,
    ) -> Result<(), AppError> {
        Ok(())
    }
    fn upload_file_with_identity(
        &self,
        source: &Path,
        target: &ObjectPath,
        _identity: Option<UploadSourceIdentity>,
    ) -> Result<String, AppError> {
        self.upload_file(source, target)
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
    transport: StorageTransport,
    upload_root: Option<PathBuf>,
    upload_root_identity: Mutex<Option<DirectoryIdentity>>,
    active_bucket_locks: Mutex<HashMap<ThreadId, Vec<String>>>,
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
            transport: StorageTransport::new(
                cloud,
                api_base,
                upload_base,
                token,
                request_timeout,
                upload_timeout,
            ),
            upload_root: None,
            upload_root_identity: Mutex::new(None),
            active_bucket_locks: Mutex::new(HashMap::new()),
        }
    }

    fn acquire_bucket_lock(&self, bucket: &str) -> Result<BucketLock, AppError> {
        let object = ObjectPath::from_parts(bucket, BUCKET_LOCK_OBJECT);
        let token = uuid::Uuid::new_v4().simple().to_string();
        let url = with_query(
            format!("{}/b/{}/o", self.transport.upload_base(), encode(bucket)),
            [
                ("uploadType", "media"),
                ("name", BUCKET_LOCK_OBJECT),
                ("ifGenerationMatch", "0"),
            ],
        );
        let metadata: Result<MetadataResponse, AppError> = self.transport.send_json(
            self.transport
                .client()
                .post(url)
                .header(CONTENT_TYPE, "application/octet-stream")
                .header(CONTENT_LENGTH, token.len() as u64)
                .body(token.as_bytes().to_vec()),
            "Invalid Cloud Storage bucket lock response",
        );
        let metadata = match metadata {
            Err(error) if error.status() == Some(412) => {
                return Err(AppError::BucketLockConflict(Box::new(error)));
            }
            Ok(metadata) => metadata,
            Err(error) if error.reached_storage() => {
                self.transport.clear_interrupt_for_rollback();
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
        let body = self
            .transport
            .send_body(self.transport.client().get(with_query(
                object_url(self.transport.api_base(), object),
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
            .filter_map(|lock| {
                // Lock release is rollback work, so a new signal must not abort cleanup.
                self.transport.clear_interrupt_for_rollback();
                self.delete_object(&lock.object, &lock.generation).err()
            })
            .collect()
    }

    fn with_bucket_locks<T, F>(&self, buckets: &[&str], operation: F) -> Result<T, AppError>
    where
        F: FnOnce() -> Result<T, AppError>,
    {
        let mut bucket_names = buckets.to_vec();
        bucket_names.sort_unstable();
        bucket_names.dedup();

        let owner = thread::current().id();
        let held_buckets = self
            .active_bucket_locks
            .lock()
            .map_err(|_| AppError::Message("Active bucket lock state is poisoned".to_string()))?
            .get(&owner)
            .cloned()
            .unwrap_or_default();
        let buckets_to_acquire = bucket_names
            .iter()
            .copied()
            .filter(|bucket| !held_buckets.iter().any(|held| held == bucket))
            .collect::<Vec<_>>();
        if buckets_to_acquire.is_empty() {
            return operation();
        }

        let mut locks = Vec::with_capacity(buckets_to_acquire.len());
        for bucket in &buckets_to_acquire {
            match self.acquire_bucket_lock(bucket) {
                Ok(lock) => locks.push(lock),
                Err(error) => {
                    self.transport.clear_interrupt_for_rollback();
                    return Err(AppError::rollback(error, self.release_bucket_locks(&locks)));
                }
            }
        }

        if let Err(error) = self.register_active_bucket_locks(owner, &buckets_to_acquire) {
            self.transport.clear_interrupt_for_rollback();
            return Err(AppError::rollback(error, self.release_bucket_locks(&locks)));
        }

        let result = operation();
        let state_error = self.unregister_active_bucket_locks(owner, &buckets_to_acquire);
        let interrupted = self.transport.clear_interrupt_for_rollback();
        let release_errors = self.release_bucket_locks(&locks);
        if let Err(error) = state_error {
            return Err(AppError::rollback(error, release_errors));
        }
        match result {
            Ok(value) if release_errors.is_empty() && !interrupted => Ok(value),
            Ok(_) if release_errors.is_empty() => Err(AppError::Interrupted),
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

    fn register_active_bucket_locks(
        &self,
        owner: ThreadId,
        buckets: &[&str],
    ) -> Result<(), AppError> {
        let mut active = self
            .active_bucket_locks
            .lock()
            .map_err(|_| AppError::Message("Active bucket lock state is poisoned".to_string()))?;
        active
            .entry(owner)
            .or_default()
            .extend(buckets.iter().map(|bucket| (*bucket).to_string()));
        Ok(())
    }

    fn unregister_active_bucket_locks(
        &self,
        owner: ThreadId,
        buckets: &[&str],
    ) -> Result<(), AppError> {
        let mut active = self
            .active_bucket_locks
            .lock()
            .map_err(|_| AppError::Message("Active bucket lock state is poisoned".to_string()))?;
        if let Some(held) = active.get_mut(&owner) {
            for bucket in buckets {
                if let Some(index) = held.iter().position(|held| held == bucket) {
                    held.remove(index);
                }
            }
            if held.is_empty() {
                active.remove(&owner);
            }
        }
        Ok(())
    }

    fn object_metadata(
        &self,
        object: &ObjectPath,
        generation: Option<&str>,
    ) -> Result<ObjectMetadata, AppError> {
        let mut url = object_url(self.transport.api_base(), object);
        if let Some(generation) = generation {
            url = with_query(url, [("generation", generation)]);
        }
        let metadata: MetadataResponse = self.transport.send_json(
            self.transport.client().get(url),
            "Invalid Cloud Storage metadata",
        )?;
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
        let base_url = rewrite_url(self.transport.api_base(), source, target);
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
                .transport
                .send_json(
                    self.transport.client().post(url),
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
                    AppError::StorageResponse(
                        "Cloud Storage rewrite omitted its resource".to_string(),
                    )
                })?;
                return Ok(resource.generation);
            }
            rewrite_token = Some(rewrite.rewrite_token.ok_or_else(|| {
                AppError::StorageResponse(
                    "Cloud Storage rewrite omitted its continuation token".to_string(),
                )
            })?);
        }
    }

    fn delete_object(&self, object: &ObjectPath, generation: &str) -> Result<(), AppError> {
        let url = with_query(
            object_url(self.transport.api_base(), object),
            [("generation", generation)],
        );
        self.transport
            .send_body(self.transport.client().delete(url))
            .map(|_| ())
    }

    fn confirm_object_generation(
        &self,
        object: &ObjectPath,
        expected_generation: &str,
        operation: &str,
    ) -> Result<(), AppError> {
        // A copied object already changed remote state, so verification token failures must remain recoverable.
        let details = self
            .object_details(object)
            .map_err(AppError::mark_reached_storage);
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
        self.with_bucket_locks(&[bucket], || self.list_objects_unlocked(bucket))
    }

    fn with_bucket_locks<T, F>(&self, buckets: &[&str], operation: F) -> Result<T, AppError>
    where
        F: FnOnce() -> Result<T, AppError>,
    {
        StorageApi::with_bucket_locks(self, buckets, operation)
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
        self.upload_file_checked(source, target, None)
    }

    fn upload_file_with_identity(
        &self,
        source: &Path,
        target: &ObjectPath,
        identity: Option<UploadSourceIdentity>,
    ) -> Result<String, AppError> {
        self.upload_file_checked(source, target, identity)
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
    fn upload_file_checked(
        &self,
        source: &Path,
        target: &ObjectPath,
        expected_source: Option<UploadSourceIdentity>,
    ) -> Result<String, AppError> {
        Self::reject_bucket_lock_object(target)?;
        let expected_root = *self
            .upload_root_identity
            .lock()
            .map_err(|_| AppError::Message("Upload root identity lock is poisoned".to_string()))?;
        let file = upload_source::open(
            self.upload_root.as_deref(),
            source,
            expected_root,
            expected_source,
        )?;
        let size = file.metadata().map_err(AppError::UploadSource)?.len();
        self.with_bucket_locks(&[target.bucket.as_str()], || {
            let url = with_query(
                format!(
                    "{}/b/{}/o",
                    self.transport.upload_base(),
                    encode(&target.bucket)
                ),
                [
                    ("uploadType", "media"),
                    ("name", target.object.as_str()),
                    ("ifGenerationMatch", "0"),
                ],
            );
            let request = self
                .transport
                .upload_client()
                .post(url)
                .header(CONTENT_TYPE, "application/octet-stream")
                .header(CONTENT_LENGTH, size)
                .body(Body::wrap_stream(ReaderStream::new(
                    tokio::fs::File::from_std(file),
                )));
            let metadata: MetadataResponse = self
                .transport
                .send_json(request, "Invalid Cloud Storage upload response")?;
            Ok(metadata.generation)
        })
    }
}

impl StorageApi {
    fn list_objects_unlocked(&self, bucket: &str) -> Result<Vec<String>, AppError> {
        let mut page_token: Option<String> = None;
        let mut objects = Vec::new();
        loop {
            let mut query = vec![("maxResults", "1000")];
            if let Some(page_token) = &page_token {
                query.push(("pageToken", page_token.as_str()));
            }
            let url = with_query(
                format!("{}/b/{}/o", self.transport.api_base(), encode(bucket)),
                query,
            );
            let listing: ListResponse = self.transport.send_json(
                self.transport.client().get(url),
                "Invalid Cloud Storage list response",
            )?;
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
#[path = "../tests/unit/storage.rs"]
mod tests;
