-- Una URL configurada siempre identifica al mismo registro firmado.
CREATE UNIQUE INDEX adapter_registry_checkpoint_url_unique
  ON adapter_registry_checkpoint(registry_url);
