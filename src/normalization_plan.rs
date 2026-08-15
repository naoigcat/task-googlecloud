use std::collections::BTreeMap;

use unicode_normalization::UnicodeNormalization;

use crate::error::AppError;

#[derive(Clone, Debug, PartialEq, Eq)]
/// Maps one discovered name to the NFC name that the workflow will use.
pub struct Entry {
    pub source: String,
    pub target: String,
}

/// Returns the NFC representation used for Cloud Storage object names.
pub fn normalized(value: &str) -> String {
    value.nfc().collect()
}

/// Builds a complete no-side-effect plan and rejects normalized-name collisions
/// before any local rename or remote request can begin.
pub fn build(names: &[String]) -> Result<Vec<Entry>, AppError> {
    let entries = names
        .iter()
        .map(|name| Entry {
            source: name.clone(),
            target: normalized(name),
        })
        .collect::<Vec<_>>();

    // Grouping before execution prevents two distinct source objects from
    // claiming the same normalized destination.
    let mut sources_by_target = BTreeMap::<&str, Vec<&str>>::new();
    for entry in &entries {
        sources_by_target
            .entry(&entry.target)
            .or_default()
            .push(&entry.source);
    }

    let collisions = sources_by_target
        .into_iter()
        .filter(|(_, sources)| sources.len() > 1)
        .map(|(target, _)| target.to_string())
        .collect::<Vec<_>>();
    if !collisions.is_empty() {
        return Err(AppError::Collision(collisions.join(", ")));
    }

    Ok(entries)
}
