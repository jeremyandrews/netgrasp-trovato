-- AI search is for signed-in users too. Forward-only; no rollback. Safe to run
-- twice.
--
-- Kernel 0.105.0 (Trovato main, 20261130000001_grant_use_ai_to_searchers.sql)
-- gave the AI search endpoints (`/api/v1/search/expand`, `/summarize`,
-- `/followup`) a permission of their own, `use ai`, and its migration grants it
-- to every role holding `search content`. Kernel migrations run before plugin
-- migrations, so on a new install the anonymous role still holds `search
-- content` at that moment (026 has not run yet) and comes away holding `use
-- ai`. On an upgrade of a site where 026 already ran it does not, and this
-- deletes nothing.
--
-- Anonymous visitors cannot search (026), and every netgrasp path sends them to
-- sign in, so `use ai` left with them would only let a visitor with no session
-- spend the site's AI provider tokens. The anonymous role loses it here, and a
-- site opening up grants it back with the others (README, "Who sees device
-- data").
DELETE FROM role_permissions
WHERE role_id = '00000000-0000-0000-0000-000000000001'
  AND permission = 'use ai';
