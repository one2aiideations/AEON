//! The roles a device starts with, defined once.

use mouli_interface::{Permissions, Workspace};

/// May do everything and decide approvals.
pub const OWNER: &str = "owner";
/// May read and propose or reject code changes, but not apply them or decide approvals.
pub const CLERK: &str = "clerk";
/// May only look.
pub const VIEWER: &str = "viewer";

/// Adds the owner, clerk and viewer roles. Applications register their own capabilities
/// separately; roles name them by prefix so new ones need no change here.
pub fn add_default_roles(ws: &mut Workspace) {
    ws.add_role(Permissions::new(OWNER, &["*"]));
    ws.add_role(Permissions::new(
        CLERK,
        &[
            "code.status",
            "code.files",
            "code.read",
            "code.propose",
            "code.reject",
        ],
    ));
    ws.add_role(Permissions::new(
        VIEWER,
        &["code.status", "code.files", "code.read"],
    ));
}
