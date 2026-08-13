use task_googlecloud::AppError;

#[test]
fn missing_generation_errors_are_not_silently_ignored() {
    let error = AppError::MissingGeneration("gs://bucket/object".to_string());
    assert_eq!(
        error.to_string(),
        "Cannot verify ownership for \"gs://bucket/object\" without a generation"
    );
}
