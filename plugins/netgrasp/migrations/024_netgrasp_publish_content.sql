-- network_admin may publish what it creates. Forward-only; no rollback. Safe to
-- run twice.
--
-- Kernel 0.107 checks `publish content` on every item write that sets or
-- changes `status`, including a plugin's own `save-item` made on a request
-- path, which now acts as the signed-in user. A person created from the owner
-- form or by the assistant, and a device Item minted by a rename, are saved
-- published, so a network_admin without the permission gets
-- ERR_ITEM_ACCESS_DENIED (-60) and "Nothing changed".
--
-- The kernel's own migration (20261004000001_grant_publish_content) grants it
-- to every role that already holds a `create ... content` permission, so on an
-- install that existed before the kernel moved, network_admin already has it.
-- On a new install the kernel migrates before any plugin does and network_admin
-- (003) does not exist yet, so nothing grants it. This does.
--
-- network_viewer stays read-only: it holds no create or edit permission, and
-- publishing is a write.
INSERT INTO role_permissions (role_id, permission)
SELECT r.id, 'publish content'
FROM roles r
WHERE r.name = 'network_admin'
ON CONFLICT DO NOTHING;
