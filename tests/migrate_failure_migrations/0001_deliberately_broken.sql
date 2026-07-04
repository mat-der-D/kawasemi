-- Test-only migration used solely by tests/migrate_it.rs to exercise the
-- apply-failure path (Requirement 4.5). It is embedded by a test-local
-- `sqlx::migrate!` invocation pointed at this directory, never by the
-- production `sqlx::migrate!("./migrations")` call in `src/migrate.rs`,
-- so it never runs against a production database.
INSERT INTO this_table_does_not_exist (col) VALUES (1);
