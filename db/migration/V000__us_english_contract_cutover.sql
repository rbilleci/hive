-- Schema objects use US-English names: "organization", not "organisation"; "catalog", not
-- "catalogue"; "color", not "colour". Nothing renames them, so a new object must be named that way
-- from the start.
--
-- This file holds no DDL. It cannot: Aurora DSQL rejects `DO ... LANGUAGE plpgsql` outright, and the
-- migrator replays V000 on every startup without a ledger entry, so anything here must be idempotent
-- against an already-migrated database. The statement below exists because every step in
-- `hive_persistence::migrator::steps` must carry SQL.
SELECT 1;
