# ovis-ocr

Local OCR on `127.0.0.1:8797`: OvisOCR2 on mlx-vlm turns an image or a PDF into Markdown. The ovis-ocr plugin (`../plugin`) owns the widget and the chord. Moved here from the Hammerspoon setup's `apps/ovis_ocr`; `page.html` (the drop page at `/`) is that file unchanged; `server.py` gained a script block on top and `mcp.py` a corrected path in its header. The server's header lists the endpoints.

`server.py` names its packages (mlx-vlm, pillow, pypdfium2) in an inline script block, so node.toml runs it with `uv run --script server.py`: uv builds the venv on the first start and reuses it after. The model loads on the first request and unloads after ten idle minutes.

`mcp.py` is a standard-library MCP bridge that gives any MCP client one tool, `ocr_to_markdown`. It only forwards to the server on :8797:

```bash
claude mcp add --scope user ovis-ocr -- python3 /path/to/bricolo/apps/ovis-ocr/sidecar/mcp.py
curl -s 127.0.0.1:8797/health
bricolo sidecar logs ovis-ocr
```
