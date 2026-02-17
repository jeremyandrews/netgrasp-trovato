-- Clean up stale permission rows from the old kernel-inline registration.
-- The original register_netgrasp_validation() in state.rs used "edit any {type} content"
-- and "delete any {type} content" format. The correct kernel fallback format is
-- "{operation} {type} content" (no "any" qualifier). This migration removes the
-- orphaned rows so they don't clutter permission listings.
--
-- Forward-only: no rollback. The stale rows serve no purpose.

DELETE FROM role_permissions
WHERE permission LIKE 'edit any ng\_% content' ESCAPE '\'
   OR permission LIKE 'delete any ng\_% content' ESCAPE '\';
