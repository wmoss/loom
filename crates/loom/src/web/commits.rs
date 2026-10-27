use std::path::Path as FsPath;
use weaver_api::operations::sessions as ops;
use weaver_api::SessionCommitsDto;

use super::operations::{register, Bound, OperationContext};
use super::{require_session, ApiResult};

/// The `sessions.commits` operation binding, folded into the `sessions`
/// bundle by [`super::sessions::bound_operations`].
pub(super) fn bound_operations() -> Vec<Bound> {
    vec![register::<ops::commits::Op, _, _>(op_commits)]
}

async fn op_commits(
    context: OperationContext,
    input: ops::commits::Input,
) -> ApiResult<SessionCommitsDto> {
    let (session, branch) = require_session(&context.state.db, &input.session).await?;
    Ok(crate::commits::load(FsPath::new(&session.work_dir), &branch.base_branch).await?)
}
