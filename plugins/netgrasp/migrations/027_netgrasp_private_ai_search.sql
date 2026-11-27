-- AI search is for signed-in users too. Forward-only; no rollback. Safe to run
-- twice.
--
-- Trovato main (B89b, `20261130000001_grant_use_ai_to_searchers.sql`) gave the
-- AI search endpoints (`/api/v1/search/expand`, `/summarize` and `/followup`)
-- a permission of their own, `use ai`, and its migration grants it to every
-- role holding `search content`. Kernel migrations run before plugin
-- migrations, so on a new install the anonymous role still holds `search
-- content` at that moment (026 has not run yet) and comes away holding
-- `use ai`.
--
-- On a site where 026 already ran, the anonymous role no longer holds `search
-- content`, the kernel grants it nothing, and this deletes nothing.
--
-- Without `search content` an anonymous visitor cannot search, so this is not
-- a leak of device data. But `use ai` is what lets a request spend the site's
-- provider tokens, and every other path of a Netgrasp site already sends a
-- visitor with no session to sign in. So the anonymous role loses it here, and
-- a site opening up grants it back with the other three (README, "Who sees
-- device data").
DELETE FROM role_permissions
WHERE role_id = '00000000-0000-0000-0000-000000000001'
  AND permission = 'use ai';
