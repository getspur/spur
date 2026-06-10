from __future__ import annotations

from spur_app import App

from tools.get_template import html_video_get_template
from tools.render import html_video_render
from tools.search import html_video_search_templates

app = App("html-video")
app.tool()(html_video_render)
app.tool()(html_video_search_templates)
app.tool()(html_video_get_template)


def main() -> None:
    app.run()


if __name__ == "__main__":
    main()
