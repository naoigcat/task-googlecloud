use task_googlecloud::{Cloud, shell_quote};

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
    assert!(args.contains(&"cloud@googlecloud".to_string()));
    assert!(!args.contains(&"root@googlecloud".to_string()));
}
