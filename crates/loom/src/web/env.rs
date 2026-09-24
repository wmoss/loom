use weaver_api::operations::settings::env as settings_env_operations;
use weaver_api::AgentEnvVarView;

use crate::db::Db;
use crate::{agent_env, profile};

use super::operations::OperationContext;
use super::{ApiResult, AppError};

// ---------------------------------------------------------------------------
// Operator-managed agent environment variables
// ---------------------------------------------------------------------------

/// The default profile's environment, values included in full — this facade
/// predates the write-only convention profiles use.
async fn env_vars(db: &Db) -> ApiResult<Vec<AgentEnvVarView>> {
    Ok(agent_env::list(db).await?)
}

/// Cursor's catalogue probe authenticates from the stored environment, so a
/// change to its credentials must drop the cached result to take effect now
/// rather than at the next TTL expiry.
pub(super) fn invalidate_cursor_catalog_if_credential(name: &str) {
    if name == "CURSOR_API_KEY" || name == "CURSOR_AUTH_TOKEN" {
        crate::agent::invalidate_builtin_catalog("cursor-agent");
    }
}

// ---------------------------------------------------------------------------
// Operation registry — `settings.env.*`. Bound from `web/settings.rs`'s
// `bound_operations()` (the `settings` bundle owns the descriptors), since
// this facade is a sub-resource of `settings`, not its own bundle.
// ---------------------------------------------------------------------------

pub(super) async fn list_settings_env_operation(
    context: OperationContext,
    _input: settings_env_operations::list::Input,
) -> ApiResult<Vec<AgentEnvVarView>> {
    env_vars(&context.state.db).await
}

/// `settings.env.set`. The name is validated as a shell identifier so it
/// cannot corrupt the launch script that exports it; the value is free-form.
pub(super) async fn set_settings_env_operation(
    context: OperationContext,
    input: settings_env_operations::set::Input,
) -> ApiResult<Vec<AgentEnvVarView>> {
    let st = context.state;
    if let Err(why) = agent_env::validate_name(&input.name) {
        return Err(AppError::bad_request(why));
    }
    let _profile_permit = st
        .launch_gate
        .acquire_profile(profile::DEFAULT_PROFILE)
        .await;
    profile::env_set(&st.db, profile::DEFAULT_PROFILE, &input.name, &input.value).await?;
    invalidate_cursor_catalog_if_credential(&input.name);
    env_vars(&st.db).await
}

/// `settings.env.delete`. Returns the refreshed list; a missing name is not
/// an error, since the desired end state — absent — already holds.
pub(super) async fn delete_settings_env_operation(
    context: OperationContext,
    input: settings_env_operations::delete::Input,
) -> ApiResult<Vec<AgentEnvVarView>> {
    let st = context.state;
    let _profile_permit = st
        .launch_gate
        .acquire_profile(profile::DEFAULT_PROFILE)
        .await;
    profile::env_remove(&st.db, profile::DEFAULT_PROFILE, &input.name).await?;
    invalidate_cursor_catalog_if_credential(&input.name);
    env_vars(&st.db).await
}
