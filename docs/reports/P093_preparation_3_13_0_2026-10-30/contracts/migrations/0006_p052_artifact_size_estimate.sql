-- P052 contract revision 3.1.0. Preserve every legacy row as unknown until re-observed.
ALTER TABLE model_artifact
  ADD COLUMN size_estimate TEXT NOT NULL
  DEFAULT '{"confidence":"UNKNOWN","bytes":null,"lower_bound_bytes":null,"upper_bound_bytes":null,"source":"UNKNOWN","evidence_refs":["migration:0006"],"reason":"legacy size has no confidence evidence"}'
  CHECK(json_valid(size_estimate));
