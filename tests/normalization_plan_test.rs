use task_googlecloud::{AppError, Entry, build_normalization_plan};

#[test]
fn builds_nfc_targets() {
    let decomposed = "e\u{301}.txt".to_string();
    assert_eq!(
        build_normalization_plan(std::slice::from_ref(&decomposed)).unwrap(),
        vec![Entry {
            source: decomposed,
            target: "é.txt".to_string(),
        }]
    );
}

#[test]
fn rejects_normalized_collisions_before_side_effects() {
    let result = build_normalization_plan(&["e\u{301}.txt".to_string(), "é.txt".to_string()]);
    assert!(matches!(result, Err(AppError::Collision(_))));
}
