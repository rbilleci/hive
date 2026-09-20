CREATE INDEX IF NOT EXISTS projects_directory_keyset
    ON projects (organization_id, display_name, id);

CREATE INDEX IF NOT EXISTS projects_directory_lifecycle_keyset
    ON projects (organization_id, lifecycle_status, display_name, id);
