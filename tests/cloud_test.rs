use task_googlecloud::{Cloud, shell_quote};

#[test]
fn documents_ephemeral_cloud_authentication_without_a_logout_command() {
    let mise_config = std::fs::read_to_string(".mise.toml").unwrap();
    let compose = std::fs::read_to_string("compose.yaml").unwrap();
    let readme = std::fs::read_to_string("README.md").unwrap();
    let source = std::fs::read_to_string("src/cloud.rs").unwrap();

    assert!(!mise_config.contains("[tasks.logout]"));
    assert!(mise_config.contains("docker compose down"));
    assert!(!compose.contains("/home/cloud/.config/gcloud"));
    assert!(!source.contains("gcloud auth revoke"));
    assert!(readme.contains("temporary `googlecloud` container"));
    assert!(!readme.contains("mise run logout"));
}

#[test]
fn shell_quotes_project_names_as_one_remote_argument() {
    assert_eq!(
        shell_quote("project;$(touch pwned)"),
        "'project;$(touch pwned)'"
    );
}

#[test]
fn ssh_options_require_key_authentication_and_host_verification() {
    let args = Cloud::new().ssh_arguments("gcloud auth login");
    assert!(args.contains(&"StrictHostKeyChecking=yes".to_string()));
    assert!(args.contains(&"UserKnownHostsFile=/run/googlecloud-ssh/known_hosts".to_string()));
    assert!(args.contains(&"ServerAliveInterval=5".to_string()));
    assert!(args.contains(&"ServerAliveCountMax=3".to_string()));
    assert!(args.contains(&"cloud@googlecloud".to_string()));
    assert!(!args.contains(&"root@googlecloud".to_string()));
}

#[test]
fn configures_the_requested_project_before_authentication() {
    for (workflow, source) in [
        ("normalize", include_str!("../src/normalize.rs")),
        ("upload", include_str!("../src/upload.rs")),
    ] {
        let project_position = source
            .find("cloud.set_project(project)?")
            .unwrap_or_else(|| panic!("{workflow} must set the requested project"));
        let login_position = source
            .find("cloud.login()?")
            .unwrap_or_else(|| panic!("{workflow} must authenticate"));

        assert!(
            project_position < login_position,
            "{workflow} must configure the project before authentication"
        );
    }
}

#[test]
fn reports_the_upload_project_before_acquiring_bucket_locks() {
    let source = include_str!("../src/upload.rs");
    let login_position = source.find("cloud.login()?").unwrap();
    let log_position = source
        .find("println!(\"Using Google Cloud project [{project}].\");")
        .expect("upload must report the configured project");
    let lock_position = source
        .find("storage.with_bucket_locks(&buckets, ||")
        .expect("upload must acquire bucket locks");

    assert!(
        login_position < log_position && log_position < lock_position,
        "upload must report the project after authentication and before remote processing"
    );
}
