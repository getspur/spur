CREATE OR REPLACE MACRO b64_decode_lenient(s) AS (
  from_base64(replace(replace(s, '-', '+'), '_', '/') || repeat('=', (4 - length(s) % 4) % 4))
);
