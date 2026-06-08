from __future__ import annotations


def create_server():
    from mcp.server.fastmcp import FastMCP

    from tools.get_template import html_video_get_template
    from tools.render import html_video_render
    from tools.search import html_video_search_templates

    mcp = FastMCP("html-video")
    mcp.tool()(html_video_render)
    mcp.tool()(html_video_search_templates)
    mcp.tool()(html_video_get_template)
    return mcp


def main() -> None:
    create_server().run(transport="stdio")


if __name__ == "__main__":
    main()
