#[cfg(unix)]
use std::ffi::CString;
use std::fs;
use std::fs::File;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
#[cfg(unix)]
use std::path::Component;
use std::path::{Path, PathBuf};
use std::time::Duration;

use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use serde::Deserialize;
use url::form_urlencoded::Serializer;

use crate::cloud::Cloud;
use crate::error::AppError;

/// Cloud Storage rejects object names longer than this, so reject them before
/// any request is sent and the transaction is half applied.
pub(crate) const MAX_OBJECT_NAME_BYTES: usize = 1024;
const API_BASE: &str = "https://storage.googleapis.com/storage/v1";
const UPLOAD_BASE: &str = "https://storage.googleapis.com/upload/storage/v1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(60 * 60);
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

pub trait StorageClient {
    fn list_objects(&self, bucket: &str) -> Result<Vec<String>, AppError>;
    fn upload_file(&self, source: &Path, target: &ObjectPath) -> Result<String, AppError>;
    fn move_object(&self, source: &ObjectPath, target: &ObjectPath) -> Result<String, AppError>;
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
}

impl StorageApi {
    pub fn new(cloud: Cloud) -> Self {
        let mut storage = Self::with_endpoints(cloud, API_BASE, UPLOAD_BASE, None);
        storage.upload_root = Some(PathBuf::from("uploads"));
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
            .map_or_else(|| self.cloud.access_token(), Ok)
    }

    fn send(&self, request: RequestBuilder) -> Result<Response, AppError> {
        let token = self.token()?;
        Ok(request.bearer_auth(token).send()?)
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

    #[cfg(unix)]
    fn open_upload_file(&self, source: &Path) -> Result<File, AppError> {
        if let Some(root) = &self.upload_root {
            let relative = source.strip_prefix(root).map_err(|_| {
                AppError::Message(format!("Upload source is outside {root:?}: {source:?}"))
            })?;
            open_relative_without_following_links(root, relative)
        } else {
            open_without_upload_root(source)
        }
    }

    #[cfg(not(unix))]
    fn open_upload_file(&self, source: &Path) -> Result<File, AppError> {
        let metadata = fs::symlink_metadata(source)?;
        if !metadata.file_type().is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("Upload source is not a regular file: {source:?}"),
            )
            .into());
        }
        File::open(source).map_err(AppError::from)
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
        let response = self.send(self.client.get(url))?;
        let body = Self::response_body(response)?;
        let metadata: MetadataResponse = serde_json::from_slice(&body).map_err(|error| {
            AppError::Message(format!("Invalid Cloud Storage metadata: {error}"))
        })?;
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

            let response = self.send(self.client.post(url))?;
            let body = Self::response_body(response)?;
            let rewrite: RewriteResponse = serde_json::from_slice(&body).map_err(|error| {
                AppError::Message(format!("Invalid Cloud Storage rewrite response: {error}"))
            })?;
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
        let response = self.send(self.client.delete(url))?;
        Self::response_body(response).map(|_| ())
    }
}

impl StorageClient for StorageApi {
    fn list_objects(&self, bucket: &str) -> Result<Vec<String>, AppError> {
        let mut page_token: Option<String> = None;
        let mut objects = Vec::new();
        loop {
            let mut query = Serializer::new(String::new());
            query.append_pair("maxResults", "1000");
            if let Some(page_token) = &page_token {
                query.append_pair("pageToken", page_token);
            }
            let url = format!(
                "{}/b/{}/o?{}",
                self.api_base,
                encode(bucket),
                query.finish()
            );
            let response = self.send(self.client.get(url))?;
            let body = Self::response_body(response)?;
            let listing: ListResponse = serde_json::from_slice(&body).map_err(|error| {
                AppError::Message(format!("Invalid Cloud Storage list response: {error}"))
            })?;
            objects.extend(
                listing
                    .items
                    .into_iter()
                    .map(|item| format!("gs://{bucket}/{}", item.name)),
            );
            page_token = listing.next_page_token;
            if page_token.is_none() {
                return Ok(objects);
            }
        }
    }

    fn upload_file(&self, source: &Path, target: &ObjectPath) -> Result<String, AppError> {
        let file = self.open_upload_file(source)?;
        let size = file.metadata()?.len();
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
        let response = self.send(request)?;
        let body = Self::response_body(response)?;
        let metadata: MetadataResponse = serde_json::from_slice(&body).map_err(|error| {
            AppError::Message(format!("Invalid Cloud Storage upload response: {error}"))
        })?;
        Ok(metadata.generation)
    }

    fn move_object(&self, source: &ObjectPath, target: &ObjectPath) -> Result<String, AppError> {
        let source_generation = self.object_metadata(source, None)?.generation;
        let target_generation =
            self.copy_object(source, target, Some(&source_generation), Some("0"))?;
        self.delete_object(source, &source_generation)?;
        if self.object_state(source, None)? != ObjectState::Missing {
            return Err(AppError::Message(format!(
                "Source object remains after moving {}",
                source.uri()
            )));
        }
        Ok(target_generation)
    }

    fn rollback_object(
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
            self.copy_object(target, source, Some(target_generation), Some("0"))?;
        self.delete_object(target, target_generation)?;
        if self.object_state(target, None)? != ObjectState::Missing {
            return Err(AppError::Message(format!(
                "Rollback target remains: {}",
                target.uri()
            )));
        }
        Ok(source_generation)
    }

    fn cleanup_object(&self, target: &ObjectPath, target_generation: &str) -> Result<(), AppError> {
        match self.object_state(target, Some(target_generation))? {
            ObjectState::Present => self.delete_object(target, target_generation)?,
            ObjectState::Missing => {}
        }
        if self.object_state(target, None)? != ObjectState::Missing {
            return Err(AppError::Message(format!(
                "Cleanup target remains: {}",
                target.uri()
            )));
        }
        Ok(())
    }

    fn confirm_move_after_failure(
        &self,
        source: &ObjectPath,
        target: &ObjectPath,
        operation: &AppError,
    ) -> Result<(), AppError> {
        let source_details = self.object_details(source);
        let target_details = self.object_details(target);
        let no_change = matches!(source_details, Ok((ObjectState::Present, _)))
            && matches!(target_details, Ok((ObjectState::Missing, _)));
        if no_change && !matches!(operation, AppError::Http(_) | AppError::Storage { .. }) {
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
        let details = self.object_details(target);
        if matches!(details, Ok((ObjectState::Missing, _)))
            && !matches!(operation, AppError::Http(_) | AppError::Storage { .. })
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

#[cfg(unix)]
fn open_relative_without_following_links(root: &Path, source: &Path) -> Result<File, AppError> {
    let directory = open_directory(root)?;
    open_path_components(directory, source, false)
}

#[cfg(unix)]
fn open_without_upload_root(source: &Path) -> Result<File, AppError> {
    let metadata = fs::symlink_metadata(source)?;
    if !metadata.file_type().is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Upload source is not a regular file: {source:?}"),
        )
        .into());
    }
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)
        .map_err(AppError::from)
}

#[cfg(unix)]
fn open_path_components(
    mut directory: File,
    source: &Path,
    allow_root: bool,
) -> Result<File, AppError> {
    let mut components = source.components().peekable();

    while let Some(component) = components.next() {
        match component {
            Component::RootDir if allow_root => {}
            Component::CurDir => {}
            Component::RootDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Upload source is absolute: {source:?}"),
                )
                .into());
            }
            Component::ParentDir => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Upload source contains a parent path component: {source:?}"),
                )
                .into());
            }
            Component::Normal(name) if components.peek().is_some() => {
                let fd = open_at(directory.as_raw_fd(), name, directory_flags())?;
                directory = unsafe { File::from_raw_fd(fd) };
            }
            Component::Normal(name) => {
                let fd = open_at(directory.as_raw_fd(), name, file_flags())?;
                let file = unsafe { File::from_raw_fd(fd) };
                if !file.metadata()?.file_type().is_file() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("Upload source is not a regular file: {source:?}"),
                    )
                    .into());
                }
                return Ok(file);
            }
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("Upload source is not a regular file: {source:?}"),
                )
                .into());
            }
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("Upload source is not a regular file: {source:?}"),
    )
    .into())
}

#[cfg(unix)]
fn open_directory(path: &Path) -> Result<File, AppError> {
    let fd = open_at(libc::AT_FDCWD, path.as_os_str(), directory_flags())?;
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_at(directory: RawFd, name: &std::ffi::OsStr, flags: i32) -> Result<RawFd, AppError> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| AppError::Message(format!("Path contains NUL: {name:?}")))?;
    let fd = unsafe { libc::openat(directory, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(fd)
}

#[cfg(unix)]
fn directory_flags() -> i32 {
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
}

#[cfg(unix)]
fn file_flags() -> i32 {
    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW
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
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use tempfile::NamedTempFile;

    use super::{Cloud, Duration, ObjectPath, StorageApi, StorageClient};

    #[test]
    fn uploads_files_with_a_longer_timeout_than_api_requests() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base = format!("http://{}/storage/v1", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut buffer).unwrap();
                request.push(buffer[0]);
            }
            thread::sleep(Duration::from_millis(100));
            let body = r#"{"generation":"456"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
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
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut buffer).unwrap();
                request.push(buffer[0]);
            }
            headers_sent.send(()).unwrap();
            release_receiver.recv().unwrap();
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
        let result = result_receiver.recv_timeout(Duration::from_secs(1));
        release_sender.send(()).unwrap();
        upload.join().unwrap();
        server.join().unwrap();

        let error = result.unwrap().unwrap_err();

        assert!(matches!(error, super::AppError::Http(_)));
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

        assert!(matches!(error, super::AppError::Io(_)), "{error}");
    }
}
