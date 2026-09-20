-- M12UXC extends existing safe metadata into inert MCP server descriptors.
-- No column stores a credential value, raw header/environment value, health result, or executable state.
--
-- Aurora DSQL rejects any constraint (NOT NULL, DEFAULT, or CHECK) inline on ADD COLUMN outright
-- (confirmed against the real hive-dsql-verification cluster: "ALTER TABLE ADD COLUMN with
-- constraint not supported", even for a bare DEFAULT with no CHECK), and has no ALTER COLUMN ...
-- SET NOT NULL at all ("unsupported ALTER TABLE ALTER COLUMN ... SET NOT NULL statement"). Every
-- column below is added bare, then a default and a NOT NULL-equivalent CHECK are attached as
-- separate statements - see DatabaseMigrator.runStatement()'s comment for how the CHECK statements
-- reach Aurora DSQL's required NOT VALID + VALIDATE CONSTRAINT form automatically.
-- stdio_arguments/redacted_bindings/declared_tools/declared_resources/declared_prompts are JSONB
-- (each holding a JSON array of strings), not TEXT[]: Aurora DSQL does not support array types at all
-- (confirmed against the real hive-dsql-verification cluster: "datatype text[] not supported"). See
-- PostgresConfigurationRepository's textArray()/array() helpers for the Java-side (de)serialization
-- this requires.
ALTER TABLE project_tool_connections
    ADD COLUMN IF NOT EXISTS server_id TEXT,
    ADD COLUMN IF NOT EXISTS enabled BOOLEAN,
    ADD COLUMN IF NOT EXISTS transport_type TEXT,
    ADD COLUMN IF NOT EXISTS stdio_command TEXT,
    ADD COLUMN IF NOT EXISTS stdio_arguments JSONB,
    ADD COLUMN IF NOT EXISTS remote_url TEXT,
    ADD COLUMN IF NOT EXISTS redacted_bindings JSONB,
    ADD COLUMN IF NOT EXISTS declared_tools JSONB,
    ADD COLUMN IF NOT EXISTS declared_resources JSONB,
    ADD COLUMN IF NOT EXISTS declared_prompts JSONB;

ALTER TABLE project_tool_connections ALTER COLUMN enabled SET DEFAULT TRUE;
ALTER TABLE project_tool_connections ALTER COLUMN stdio_arguments SET DEFAULT '[]'::jsonb;
ALTER TABLE project_tool_connections ALTER COLUMN redacted_bindings SET DEFAULT '[]'::jsonb;
ALTER TABLE project_tool_connections ALTER COLUMN declared_tools SET DEFAULT '[]'::jsonb;
ALTER TABLE project_tool_connections ALTER COLUMN declared_resources SET DEFAULT '[]'::jsonb;
ALTER TABLE project_tool_connections ALTER COLUMN declared_prompts SET DEFAULT '[]'::jsonb;

UPDATE project_tool_connections SET enabled = TRUE WHERE enabled IS NULL;
UPDATE project_tool_connections SET stdio_arguments = '[]'::jsonb WHERE stdio_arguments IS NULL;
UPDATE project_tool_connections SET redacted_bindings = '[]'::jsonb WHERE redacted_bindings IS NULL;
UPDATE project_tool_connections SET declared_tools = '[]'::jsonb WHERE declared_tools IS NULL;
UPDATE project_tool_connections SET declared_resources = '[]'::jsonb WHERE declared_resources IS NULL;
UPDATE project_tool_connections SET declared_prompts = '[]'::jsonb WHERE declared_prompts IS NULL;

ALTER TABLE project_tool_connections ADD CONSTRAINT project_tool_connections_enabled_not_null CHECK (enabled IS NOT NULL);
ALTER TABLE project_tool_connections ADD CONSTRAINT project_tool_connections_stdio_arguments_not_null CHECK (stdio_arguments IS NOT NULL);
ALTER TABLE project_tool_connections ADD CONSTRAINT project_tool_connections_redacted_bindings_not_null CHECK (redacted_bindings IS NOT NULL);
ALTER TABLE project_tool_connections ADD CONSTRAINT project_tool_connections_declared_tools_not_null CHECK (declared_tools IS NOT NULL);
ALTER TABLE project_tool_connections ADD CONSTRAINT project_tool_connections_declared_resources_not_null CHECK (declared_resources IS NOT NULL);
ALTER TABLE project_tool_connections ADD CONSTRAINT project_tool_connections_declared_prompts_not_null CHECK (declared_prompts IS NOT NULL);

UPDATE project_tool_connections
SET server_id = 'legacy-' || left (replace(id::text, '-', ''), 16)
WHERE server_id IS NULL;
UPDATE project_tool_connections
SET redacted_bindings = jsonb_build_array(redacted_secret_reference)
WHERE jsonb_array_length(redacted_bindings) = 0;
ALTER TABLE project_tool_connections
    ADD CONSTRAINT project_tool_connections_server_id_not_null CHECK (server_id IS NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS project_tool_connections_server_id
    ON project_tool_connections (project_id, server_id);

-- cardinality()/array_to_string() are array-only functions; stdio_arguments and redacted_bindings are
-- now JSONB (see this file's earlier comment on why), so the equivalent checks use
-- jsonb_array_length() and jsonb_path_exists() with a per-element like_regex predicate instead of
-- joining every element into one string first - array_to_string's join has no JSONB equivalent
-- (and reaching for a subquery to reconstruct one is rejected outright: "cannot use subquery in check
-- constraint", confirmed against the real hive-dsql-verification cluster, a standard PostgreSQL CHECK
-- constraint restriction, not one specific to Aurora DSQL). Checking each element in place is exactly
-- equivalent for a per-argument "does no element look like a credential" test, and jsonpath's
-- like_regex uses its own regex dialect (SQL standard XQuery, not POSIX): [^=:] replaces
-- [^=:[:space:]] (no POSIX classes; harmless here since arguments are stored as discrete elements
-- rather than a whitespace-joined string, so a whitespace boundary was never a meaningful signal
-- once per-element checking is possible), and "flag i" replaces the ~* operator's case-insensitivity.
ALTER TABLE project_tool_connections DROP CONSTRAINT IF EXISTS project_tool_connections_transport_check;
ALTER TABLE project_tool_connections
    ADD CONSTRAINT project_tool_connections_transport_check CHECK (
        (transport_type IS NULL AND stdio_command IS NULL AND jsonb_array_length(stdio_arguments) = 0 AND remote_url IS NULL)
            OR (transport_type = 'STDIO' AND length(btrim(stdio_command)) > 0
            AND remote_url IS NULL)
            OR (transport_type = 'REMOTE' AND stdio_command IS NULL AND jsonb_array_length(stdio_arguments) = 0
            AND remote_url ~ '^https://[^[:space:]]+$')
        );
ALTER TABLE project_tool_connections DROP CONSTRAINT IF EXISTS project_tool_connections_no_secret_arguments_check;
ALTER TABLE project_tool_connections
    ADD CONSTRAINT project_tool_connections_no_secret_arguments_check CHECK (
        NOT jsonb_path_exists(stdio_arguments,
            '$[*] ? (@ like_regex "^-*[^=:]*(password|passwd|secret|token|api[-_]?key|authorization|credential|bearer|basic|header|env|environment)[^=:]*($|[=:])" flag "i")')
    );
ALTER TABLE project_tool_connections DROP CONSTRAINT IF EXISTS project_tool_connections_no_remote_credentials_check;
ALTER TABLE project_tool_connections
    ADD CONSTRAINT project_tool_connections_no_remote_credentials_check CHECK (
        remote_url IS NULL OR (
            remote_url !~* '^https://[^/]*@'
    AND remote_url !~* '[?&][^=&#]*(password|passwd|secret|token|api[-_]?key|authorization|credential|bearer|basic|header|env|environment)[^=&#]*(=|&|#|$)'
            )
        );
ALTER TABLE project_tool_connections DROP CONSTRAINT IF EXISTS project_tool_connections_redacted_bindings_check;
ALTER TABLE project_tool_connections
    ADD CONSTRAINT project_tool_connections_redacted_bindings_check CHECK (
        jsonb_array_length(redacted_bindings) = 0
            OR
        NOT jsonb_path_exists(redacted_bindings, '$[*] ? (!(@ like_regex "^redacted://[a-z0-9/_-]{3,180}$"))')
    );
