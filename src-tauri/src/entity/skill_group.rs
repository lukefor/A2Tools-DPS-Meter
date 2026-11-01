//! Skill tables taken from the game data.

use std::collections::HashSet;

static RESOURCE_RESTORES: std::sync::LazyLock<HashSet<i32>> = std::sync::LazyLock::new(|| {
    #[derive(serde::Deserialize)]
    struct Table {
        skills: Vec<i32>,
    }
    serde_json::from_str::<Table>(include_str!("../../../src/data/resource_restore_skills.json"))
        .map(|t| t.skills.into_iter().collect())
        .unwrap_or_default()
});

/// Whether a raw skill id restores only MP (or another resource), never HP.
/// Its records carry the amount like a heal does.
pub fn restores_resource(raw: i32) -> bool {
    RESOURCE_RESTORES.contains(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_resource_restore_table_loads() {
        assert!(restores_resource(16990002), "Water Spirit's MP restore");
        assert!(!restores_resource(16990003), "Wind Spirit's restore is HP");
    }
}
