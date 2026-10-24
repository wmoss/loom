//! The uniform ACP model catalogue: one store, one probe, one reconcile.

mod probe;
mod reconcile;
pub mod selectors;
mod store;
mod succession;

pub use probe::LiveSession;
pub use reconcile::{spawn_refresh, write_through, Trigger};
pub use selectors::{normalize, NormalizedSelectors};
pub(crate) use succession::successor;

use crate::agent::AgentChoice;
use crate::db::Db;

/// The model a profile-saved id should launch as. When the catalogue lists
/// this harness and that id is gone, this is the newest listed id in the same
/// family. An empty catalogue leaves the id as given. Call this only for a
/// model saved on the profile.
pub async fn model_successor(db: &Db, harness: &str, model: &str) -> String {
    if model.is_empty() {
        return String::new();
    }
    let Ok(rows) = store::list(db, harness).await else {
        return model.to_string();
    };
    if rows.is_empty() {
        return model.to_string();
    }
    let listed: Vec<&str> = rows.iter().map(|row| row.model_id.as_str()).collect();
    successor(model, &listed).unwrap_or_else(|| model.to_string())
}

/// Replace a builtin's code-shipped model list with the stored catalogue.
///
/// An empty or unreadable store leaves the code-shipped lists in place, so a
/// harness that cannot accept a typed id still has its static choices until a
/// reconcile lands. Stored rows replace both lists.
pub async fn apply_stored(
    db: &Db,
    kind: &str,
    models: &mut Vec<AgentChoice>,
    efforts: &mut Vec<AgentChoice>,
) {
    let Ok(rows) = store::list(db, kind).await else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    efforts.clear();
    *models = rows
        .into_iter()
        .map(|row| {
            let normalized = row
                .options()
                .map(|options| normalize(&options))
                .unwrap_or_default();
            let label = if row.label.is_empty() {
                row.model_id.clone()
            } else {
                row.label.clone()
            };
            AgentChoice {
                id: row.model_id,
                label,
                efforts: normalized
                    .effort
                    .into_iter()
                    .map(|choice| AgentChoice::leaf(choice.id, choice.label))
                    .collect(),
                fast: normalized
                    .fast
                    .into_iter()
                    .map(|choice| AgentChoice::leaf(choice.id, choice.label))
                    .collect(),
            }
        })
        .collect();
}

pub async fn run(db: Db) {
    reconcile::run(db).await;
}
