-- Searching is for signed-in users too. Forward-only; no rollback. Safe to run
-- twice.
--
-- Kernel 0.107 (Trovato main, B160) gave search a permission of its own,
-- `search content`, and its migration grants it to every role holding
-- `access content`. Kernel migrations run before plugin migrations, so on an
-- upgrade, and on a new install, the anonymous role still holds `access
-- content` at that moment (025 has not run yet, or the kernel default granted
-- it), and comes away holding `search content`.
--
-- With 025 an anonymous search already finds nothing (measured: 200 and an
-- empty list), so this is not a leak. But it leaves the search page and
-- `/api/v1/search` answering a visitor with no session, where
-- every other path sends them to sign in, and the grant would outlive a site
-- deciding to open content again. So the anonymous role loses it here, and a
-- site opening up grants it back with the other two (README, "Who sees device
-- data").
DELETE FROM role_permissions
WHERE role_id = '00000000-0000-0000-0000-000000000001'
  AND permission = 'search content';
