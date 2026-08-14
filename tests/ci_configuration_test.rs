use std::fs;

#[test]
fn dependency_checks_are_required_in_ci_and_documented() {
    let root = env!("CARGO_MANIFEST_DIR");
    let workflow = fs::read_to_string(format!("{root}/.github/workflows/lint-and-test.yml"))
        .expect("GitHub Actions workflow should be readable");
    let readme =
        fs::read_to_string(format!("{root}/README.md")).expect("README should be readable");

    let lint_job = workflow
        .split_once("\n  test:\n")
        .map(|(lint_job, _)| lint_job)
        .expect("workflow should define a separate test job");

    assert!(lint_job.contains("run: mise run audit"));
    assert!(lint_job.contains("run: mise run deny"));
    assert!(readme.contains("mise run audit"));
    assert!(readme.contains("mise run deny"));
}
