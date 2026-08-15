use std::fs;

#[test]
fn dependency_checks_are_required_in_ci_and_documented() {
    let root = env!("CARGO_MANIFEST_DIR");
    let audit_workflow = fs::read_to_string(format!("{root}/.github/workflows/audit.yml"))
        .expect("GitHub Actions audit workflow should be readable");
    let deny_workflow = fs::read_to_string(format!("{root}/.github/workflows/deny.yml"))
        .expect("GitHub Actions deny workflow should be readable");
    let test_workflow = fs::read_to_string(format!("{root}/.github/workflows/test.yml"))
        .expect("GitHub Actions test workflow should be readable");
    let readme =
        fs::read_to_string(format!("{root}/README.md")).expect("README should be readable");

    assert!(audit_workflow.contains("run: mise run audit"));
    assert!(deny_workflow.contains("run: mise run deny"));
    assert!(test_workflow.contains("run: mise run test"));
    assert!(readme.contains("mise run audit"));
    assert!(readme.contains("mise run deny"));
}

#[test]
fn shellcheck_covers_the_cloud_entrypoint_in_ci() {
    let root = env!("CARGO_MANIFEST_DIR");
    let mise = fs::read_to_string(format!("{root}/.mise.toml"))
        .expect("mise configuration should be readable");
    let workflow = fs::read_to_string(format!("{root}/.github/workflows/shellcheck.yml"))
        .expect("GitHub Actions shellcheck workflow should be readable");

    assert!(mise.contains("docker compose build app googlecloud"));
    assert!(mise.contains(
        "docker compose run --rm -T --no-deps --entrypoint cat googlecloud /usr/local/bin/googlecloud-entrypoint"
    ));
    assert!(mise.contains("docker compose run --rm -T --no-deps app shellcheck -"));
    assert!(workflow.contains("run: mise run shellcheck"));
}
