-- Signed, Git-owned local fixture. It contains definition metadata only, never cloud credentials.
INSERT INTO catalog_releases (id, source, source_digest, released_at, current)
SELECT 'local-2026-08-10',
       'git:fixtures/catalog/local-2026-08-10',
       'da3212e2a83c18fcd1fc8494c20fe5688d63d9c53ea9e9a7a0f9ef1b27d903b7',
       TIMESTAMPTZ '2026-08-10 00:00:00+00', TRUE WHERE NOT EXISTS (SELECT 1 FROM catalog_releases WHERE id = 'local-2026-08-10');
INSERT INTO catalog_projection_heads (id, release_id)
SELECT 'local',
       'local-2026-08-10' WHERE NOT EXISTS (SELECT 1 FROM catalog_projection_heads WHERE id = 'local');
INSERT INTO catalog_environments (release_id, environment)
SELECT value.release_id, value.environment
FROM (VALUES
    ('local-2026-08-10', 'DEVELOPMENT'), ('local-2026-08-10', 'STAGING'), ('local-2026-08-10', 'PRODUCTION')
    ) AS value (release_id, environment)
WHERE NOT EXISTS (SELECT 1 FROM catalog_environments existing WHERE existing.release_id = value.release_id
  AND existing.environment = value.environment);
INSERT INTO catalog_definitions (release_id, definition_kind, identity, version, display_name, content_digest,
                                 available_environments)
SELECT value.release_id, value.definition_kind, value.identity, value.version, value.display_name, value.content_digest, value.available_environments
FROM (VALUES
    ('local-2026-08-10', 'model', 'local-reasoner', 'v1', 'Local Reasoner', 'aeb63686d4f3e3f5e6bc8154ce3b6fd47cb5b4858c64534d4c2297514ec2a318', '["DEVELOPMENT", "STAGING"]'::jsonb), ('local-2026-08-10', 'model', 'local-safe-chat', 'v2', 'Local Safe Chat', '6e838d62571e4b88ab061d39c95f6277821c83cb27aeb66b451017555570c2d2', '["DEVELOPMENT", "STAGING", "PRODUCTION"]'::jsonb), ('local-2026-08-10', 'tool', 'http-metadata', 'v1', 'HTTP Metadata Connector', 'af0ee9041a37ad3e121b2a077f0d44ba5ee79d87430502d748a0cc4cc418ee91', '["DEVELOPMENT", "STAGING", "PRODUCTION"]'::jsonb)
    ) AS value (release_id, definition_kind, identity, version, display_name, content_digest, available_environments)
WHERE NOT EXISTS (SELECT 1 FROM catalog_definitions existing WHERE existing.release_id = value.release_id
  AND existing.definition_kind = value.definition_kind
  AND existing.identity = value.identity
  AND existing.version = value.version);
