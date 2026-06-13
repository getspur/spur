"""code-graph-workbench - evidence-tool MCP server (seed).

wb_* evidence tools (blast_radius, subgraph, scorecard, cochange) land in
the follow-up epic; this seed pins the app contract and test harness.
"""
from spur_app import App

app = App("code-graph-workbench")


@app.tool()
def wb_ping() -> dict:
    """Smoke tool: verifies the plugin surface is live."""
    return {"ok": True, "app": "code-graph-workbench"}


if __name__ == "__main__":
    app.run()
