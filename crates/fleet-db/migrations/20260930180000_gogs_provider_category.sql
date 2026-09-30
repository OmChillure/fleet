-- The named-connection seed shipped before Gogs was on main.
INSERT INTO integration_provider_categories (provider, category_id)
VALUES ('gogs', 'source_control')
ON CONFLICT (provider) DO NOTHING;
