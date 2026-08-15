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
