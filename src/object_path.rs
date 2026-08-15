use crate::error::AppError;

/// Cloud Storage rejects object names longer than this, so reject them before
/// any request is sent and the transaction is half applied.
pub(crate) const MAX_OBJECT_NAME_BYTES: usize = 1024;

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
        Ok(Self::from_parts(bucket, object))
    }

    pub(crate) fn from_parts(bucket: &str, object: &str) -> Self {
        Self {
            bucket: bucket.to_string(),
            object: object.to_string(),
        }
    }

    pub(crate) fn validate_name_length(&self, purpose: &str) -> Result<(), AppError> {
        if self.object.len() > MAX_OBJECT_NAME_BYTES {
            return Err(AppError::Message(format!(
                "Object name is too long for {purpose}: {}",
                self.uri()
            )));
        }
        Ok(())
    }

    pub fn uri(&self) -> String {
        format!("gs://{}/{}", self.bucket, self.object)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_names_at_the_cloud_storage_limit() {
        let path = ObjectPath::from_parts("bucket", &"a".repeat(MAX_OBJECT_NAME_BYTES));

        assert!(path.validate_name_length("test").is_ok());
    }

    #[test]
    fn reports_the_validation_purpose_for_long_names() {
        let path = ObjectPath::from_parts("bucket", &"a".repeat(MAX_OBJECT_NAME_BYTES + 1));

        let error = path.validate_name_length("normalized target").unwrap_err();

        assert!(matches!(
            error,
            AppError::Message(message)
                if message.contains("normalized target") && message.contains("gs://bucket/")
        ));
    }
}
