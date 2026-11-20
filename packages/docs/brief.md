docs: read PDFs and zip-based files without extracting them. Needs exec; PDF needs poppler (pdftotext).

- `docs.pdf "file.pdf" pages:"1-2"`: text with layout kept (tables stay aligned); `raw:true` for prose.
- `docs.zip "file.zip"` lists members; `docs.unzip "file.docx" "word/document.xml"` reads one.
  Office files (.docx, .xlsx, .pptx) are zips.
- Pipe into grep/lines: `docs.pdf "invoice.pdf" | grep "Total|IBAN"`.
