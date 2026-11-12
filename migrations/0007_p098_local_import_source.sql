-- Fuente local interna usada por la importación manual de carpetas P098.
-- No carga ni ejecuta código externo; sólo registra procedencia local.
INSERT OR IGNORE INTO adapter_installation(
  id,created_at,updated_at,row_version,metadata,adapter_id,version,family,
  package_hash,publisher,trust,manifest,state
) VALUES (
  '0192f000-0000-7000-8000-00000000f101',
  '2026-10-23T00:00:00.000Z','2026-10-23T00:00:00.000Z',1,
  '{"builtin":true,"execution":"none"}',
  'larm.builtin.local-folder','1.0.0','source',
  '217e0d3ddcd68f922aeb3d47cc7d458890bb051a07fd54d5e350f5431e62fdc6',
  'LARM','VERIFIED',
  '{"adapter_id":"larm.builtin.local-folder","family":"source","version":"1.0.0","capabilities":[],"source_type":"LOCAL","execution":"none"}',
  'VALIDATED'
);

INSERT OR IGNORE INTO model_source_adapter(
  id,created_at,updated_at,row_version,metadata,adapter_installation_id,
  source_type,capabilities
) VALUES (
  '0192f000-0000-7000-8000-00000000f102',
  '2026-10-23T00:00:00.000Z','2026-10-23T00:00:00.000Z',1,
  '{"builtin":true}',
  '0192f000-0000-7000-8000-00000000f101',
  'LOCAL','[]'
);

INSERT OR IGNORE INTO model_source(
  id,created_at,updated_at,row_version,metadata,name,adapter_id,base_url,
  priority,enabled,auth_mode,account_id,policy
) VALUES (
  '0192f000-0000-7000-8000-00000000f103',
  '2026-10-23T00:00:00.000Z','2026-10-23T00:00:00.000Z',1,
  '{"builtin":true}',
  'Carpeta local',
  '0192f000-0000-7000-8000-00000000f102',
  'file:///',0,1,'NONE',NULL,'{"local_import":true}'
);
