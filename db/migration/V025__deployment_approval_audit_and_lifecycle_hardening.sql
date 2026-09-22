-- Transport correlation for a human decision is recorded on the decision itself.
ALTER TABLE deployment_approval_decisions
    ADD COLUMN IF NOT EXISTS correlation_id UUID NULL;

-- Aurora DSQL rejects CREATE TRIGGER and CREATE FUNCTION outright, so nothing copies a correlation
-- identifier into deployment_audit_events.facts. Readers use the correlation_id column, which the
-- deployment audit writer populates from the request context on every insert; the decision-recording
-- paths that also set a "correlationId" facts key set it explicitly.

-- deployment_evidence_invalidations is read-only in this system: nothing writes it, so an evidence
-- invalidation never triggers an approval handoff.
