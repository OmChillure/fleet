-- One external issue becomes one durable goal. A second intake of the
-- same issue finds this row instead of opening a duplicate crew.

ALTER TABLE workforce_goals
    ADD COLUMN source_ref TEXT;

CREATE UNIQUE INDEX workforce_goals_source_ref_idx
    ON workforce_goals (workspace_id, source_ref)
    WHERE source_ref IS NOT NULL;
