-- Kill any session left idle inside a transaction for more than a second (production databases use
-- 30s). ALTER DATABASE applies to new sessions, so this reaches the engine's pool, which connects
-- lazily on the first request (EXO_CHECK_CONNECTION_ON_STARTUP=false in the test harness).
DO $$
BEGIN
    EXECUTE format('ALTER DATABASE %I SET idle_in_transaction_session_timeout = %L', current_database(), '1s');
END
$$;

CREATE VIEW engine_session_settings AS
SELECT 1 AS id, current_setting('idle_in_transaction_session_timeout') AS value;
