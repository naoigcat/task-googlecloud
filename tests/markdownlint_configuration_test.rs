use std::fs;

#[test]
fn agents_list_markers_match_markdownlint_spacing() {
    let root = env!("CARGO_MANIFEST_DIR");
    let markdownlint = fs::read_to_string(format!("{root}/.markdownlint-cli2.jsonc"))
        .expect("Markdownlint configuration should be readable");
    let agents =
        fs::read_to_string(format!("{root}/AGENTS.md")).expect("AGENTS.md should be readable");

    let required_spaces = markdownlint
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("\"ul_single\": "))
        .and_then(|value| value.trim_end_matches(',').parse::<usize>().ok())
        .expect("Markdownlint configuration should define ul_single spacing");
    let list_item = agents
        .lines()
        .find(|line| line.contains("Place test code under"))
        .expect("AGENTS.md should document the test directory");
    let actual_spaces = list_item
        .strip_prefix('-')
        .and_then(|value| value.find(|character| character != ' '))
        .expect("AGENTS.md list item should contain text after its marker");

    assert_eq!(
        actual_spaces, required_spaces,
        "AGENTS.md should use exactly {required_spaces} spaces after unordered list markers"
    );
}
