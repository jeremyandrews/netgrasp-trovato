-- Alerts, the fifth menu entry (docs/UI.md, U3). Forward-only; no rollback.
-- Safe to run twice.
--
-- /alerts is not a gather: it is served by tap_api (src/alerts.rs), because it
-- carries forms and reads the daemon's notify tables. So there is no url_alias
-- here, only the menu row, at the weight 011 left free between People and
-- Activity.
--
-- DO NOTHING on conflict, as in 011: an operator who renamed or re-weighted the
-- entry keeps the change when this runs again.
INSERT INTO menu_link (menu_name, path, title, weight, hidden, plugin, stage_id, created, changed)
VALUES
    ('main', '/alerts', 'Alerts', 3, false, 'netgrasp', '0193a5a0-0000-7000-8000-000000000001', EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (path, menu_name, stage_id) DO NOTHING;
