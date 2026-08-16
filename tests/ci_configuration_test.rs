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

#[test]
fn app_source_is_copied_after_tool_preparation() {
    let root = env!("CARGO_MANIFEST_DIR");
    let compose = fs::read_to_string(format!("{root}/compose.yaml"))
        .expect("Compose configuration should exist");
    let tool_preparation = compose
        .find("cargo install cargo-deny --version 0.20.2 --locked")
        .expect("App tool preparation should install cargo-deny");
    let source_copy = compose
        .find("COPY src ./src")
        .expect("App Dockerfile should copy the source directory");
    let application_build = compose
        .find("RUN cargo build --release --locked")
        .expect("App Dockerfile should build the release binary");

    assert!(tool_preparation < source_copy);
    assert!(source_copy < application_build);
}

#[test]
fn actionlint_image_is_pinned_by_digest() {
    let root = env!("CARGO_MANIFEST_DIR");
    let workflow = fs::read_to_string(format!("{root}/.github/workflows/actionlint.yml"))
        .expect("GitHub Actions Actionlint workflow should be readable");

    assert!(workflow.contains(
        "uses: docker://rhysd/actionlint@sha256:b1934ee5f1c509618f2508e6eb47ee0d3520686341fec936f3b79331f9315667 # v1.7.12"
    ));
    assert!(!workflow.contains("uses: docker://rhysd/actionlint:1.7.12"));
}
