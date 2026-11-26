//! Submitted reviews of this session's own work.
//!
//! The delivery prompt is deliberately compact — subject, overall note, and
//! per-comment one-line locations with ids, without anchor context excerpts.
//! This adapter serves the durable record on demand (`list`) and takes the
//! session's own responses back (`comment_reply`); both are registered
//! `reviews.*` operations, so the catalogue, the capability sets, the
//! permission rules, and the call path are all read off [`exports`].

use std::sync::OnceLock;

use serde_json::Value;
use weaver_api::operations::reviews;

use super::dispatch::{export, Export};
use super::{Adapter, CapabilitySet, ToolFuture};

/// The tools this server exports, in the order it advertises them.
fn exports() -> &'static [Export] {
    static EXPORTS: OnceLock<Vec<Export>> = OnceLock::new();
    EXPORTS.get_or_init(|| {
        vec![
            export::<reviews::list::Op>("list"),
            export::<reviews::comments::reply::Op>("comment_reply"),
        ]
    })
}

pub(super) const ADAPTER: Adapter = Adapter {
    name: "review",
    description: "Submitted feedback on this session's artifacts and change-set.",
    capability_sets,
    exports,
    expand_tool_set,
    tools,
    call: call_boxed,
};

/// Capability sets are derived from the registry: the exported `reviews.*`
/// operations contribute their tools to the sets named by their grants.
fn capability_sets() -> &'static [CapabilitySet] {
    static SETS: OnceLock<Vec<CapabilitySet>> = OnceLock::new();
    SETS.get_or_init(|| super::dispatch::capability_sets(exports(), "review", describe_capability))
}

fn describe_capability(grant: &str) -> &'static str {
    match grant {
        "loom/reviews/read@v1" => {
            "Read submitted feedback on this session's own artifacts and change-set, complete anchors included."
        }
        "loom/reviews/write@v1" => {
            "Reply to submitted feedback on this session's own work; resolving stays with the reviewer."
        }
        _ => "Submitted review operations.",
    }
}

fn expand_tool_set(name: &str) -> Option<Vec<String>> {
    super::dispatch::expand_tool_set("review", capability_sets(), name)
}

fn tools() -> Value {
    super::dispatch::tools(exports())
}

fn call_boxed(name: &str, arguments: Value) -> ToolFuture {
    let name = name.to_string();
    Box::pin(async move {
        super::dispatch::call_adapter_tool("review", exports(), &name, arguments).await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_sets_are_grouped_by_grant() {
        let read = expand_tool_set("loom/reviews/read@v1").unwrap();
        let write = expand_tool_set("loom/reviews/write@v1").unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!(write.len(), 1);
        assert!(expand_tool_set("loom/reviews/nonexistent@v1").is_none());
    }
}
