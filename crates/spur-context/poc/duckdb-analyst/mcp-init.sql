-- Per-connection LOADs for the MotherDuck DuckDB MCP server.
-- Extensions are INSTALL-ed persistently by setup.sh / init.sql, but every
-- new connection has to LOAD them before DuckPGQ MATCH or Onager functions
-- resolve. The MCP server runs this on each connection via --init-sql.
LOAD duckpgq;
LOAD onager;
