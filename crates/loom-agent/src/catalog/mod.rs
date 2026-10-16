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

/// The model a launch should use. When the catalogue lists this harness and
/// the saved id is gone, this is the newest listed id in the same family.
/// An empty catalogue leaves the saved id alone.
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
/// An empty store leaves the lists empty: the picker shows only "Agent
/// default" until a reconcile lands, and launch does not wait on that.
pub async fn apply_stored(
    db: &Db,
    kind: &str,
    models: &mut Vec<AgentChoice>,
    efforts: &mut Vec<AgentChoice>,
) {
    efforts.clear();
    let Ok(rows) = store::list(db, kind).await else {
        models.clear();
        return;
    };
    if rows.is_empty() {
        models.clear();
        return;
    }
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
                thinking: normalized
                    .thinking
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
