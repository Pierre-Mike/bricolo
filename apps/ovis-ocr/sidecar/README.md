# ovis-ocr

Local OCR on `127.0.0.1:8797`: OvisOCR2 on mlx-vlm turns an image or a PDF into Markdown. The ovis-ocr plugin (`../plugin`) owns the widget and the chord. Moved here from the Hammerspoon setup's `apps/ovis_ocr`; `server.py`, `page.html` (the drop page at `/`) and `mcp.py` are those files unchanged. The server's header lists the endpoints.

It runs under mlx-vlm's own python (`~/.local/share/uv/tools/mlx-vlm/bin/python`). The model loads on the first request and unloads after ten idle minutes.

`mcp.py` is a standard-library MCP bridge that gives any MCP client one tool, `ocr_to_markdown`. It only forwards to the server on :8797:

```bash
claude mcp add --scope user ovis-ocr -- python3 /path/to/bricolo/apps/ovis-ocr/sidecar/mcp.py
curl -s 127.0.0.1:8797/health
bricolo sidecar logs ovis-ocr
```
