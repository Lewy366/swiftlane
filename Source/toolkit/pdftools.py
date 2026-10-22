"""PDF tools: merge, reorder, rotate, delete, split, make smaller, sign.

Uses PDFium (Chrome's PDF engine). PDFs are only read and re-assembled: nothing inside a PDF
(scripts, links, forms) is ever run. Originals are never changed; results are new files.
"""
import base64
import io
from pathlib import Path

import pypdfium2 as pdfium
from PIL import Image

MAX_THUMBS = 400
THUMB_WIDTH = 150


def _open(path):
    try:
        return pdfium.PdfDocument(str(path))
    except pdfium.PdfiumError as e:
        if "password" in str(e).lower():
            raise ValueError("This PDF is password-protected. Open it once with its password and save a copy without one.")
        raise ValueError("This file isn't a PDF Swiftlane can read.")


def thumbnails(path):
    """Page count, size and a small picture of each page (as data: URLs)."""
    pdf = _open(path)
    try:
        pages = []
        for i in range(len(pdf)):
            page = pdf[i]
            w, h = page.get_size()
            item = {"w": w, "h": h, "rotation": page.get_rotation()}
            if i < MAX_THUMBS:
                img = page.render(scale=THUMB_WIDTH / max(w, 1)).to_pil().convert("RGB")
                buf = io.BytesIO(); img.save(buf, "JPEG", quality=70)
                item["thumb"] = "data:image/jpeg;base64," + base64.b64encode(buf.getvalue()).decode()
            pages.append(item)
            page.close()
        return {"name": Path(path).name, "pages": pages}
    finally:
        pdf.close()


def page_image(path, index, width=900):
    """A bigger picture of one page, for placing a signature."""
    pdf = _open(path)
    try:
        page = pdf[int(index)]
        w, _ = page.get_size()
        img = page.render(scale=width / max(w, 1)).to_pil().convert("RGB")
        buf = io.BytesIO(); img.save(buf, "JPEG", quality=85)
        return "data:image/jpeg;base64," + base64.b64encode(buf.getvalue()).decode()
    finally:
        pdf.close()


def _signature_image(data_url):
    raw = base64.b64decode(data_url.split(",", 1)[1])
    if len(raw) > 5 * 1024 * 1024:
        raise ValueError("The signature picture is too big.")
    img = Image.open(io.BytesIO(raw))
    img.load()
    return img.convert("RGBA")


def build(sources, plan, output, signatures=()):
    """Make a new PDF from pages of `sources`.
    plan: [{"src": n, "page": i, "rotate": 0/90/180/270}, ...] in the new order.
    signatures: [{"at": index in plan, "png": data URL, "x", "y", "w"} (x/y/w as fractions of the page)]."""
    docs = [_open(p) for p in sources]
    new = pdfium.PdfDocument.new()
    try:
        for item in plan:
            src, page_no = docs[int(item["src"])], int(item["page"])
            if not 0 <= page_no < len(src):
                raise ValueError("A page is missing.")
            new.import_pages(src, pages=[page_no])
            rotate = int(item.get("rotate", 0)) % 360
            if rotate:
                page = new[len(new) - 1]
                page.set_rotation((page.get_rotation() + rotate) % 360)
                page.close()
        for sig in signatures:
            page = new[int(sig["at"])]
            pw, ph = page.get_size()
            img = _signature_image(sig["png"])
            white = Image.new("RGB", img.size, (255, 255, 255))  # PDFium images have no transparency: flatten on white
            white.paste(img, mask=img.split()[3])
            pimg = pdfium.PdfImage.new(new)
            pimg.set_bitmap(pdfium.PdfBitmap.from_pil(white))
            w = max(0.02, min(1.0, float(sig["w"]))) * pw
            h = w * img.height / max(img.width, 1)
            x = max(0.0, min(1.0, float(sig["x"]))) * pw
            y = ph - max(0.0, min(1.0, float(sig["y"]))) * ph - h   # PDF counts from the bottom
            pimg.set_matrix(pdfium.PdfMatrix().scale(w, h).translate(x, y))
            page.insert_obj(pimg)
            page.gen_content()
            page.close()
        new.save(str(output))
    finally:
        new.close()
        for d in docs:
            d.close()
    return [Path(output)]


def split(path, folder, groups):
    """groups: [[page, page, ...], ...] -> one new PDF per group, named "<name> (pages a-b).pdf"."""
    src = _open(path)
    outs = []
    try:
        stem = Path(path).stem
        for g in groups:
            pages = [int(p) for p in g if 0 <= int(p) < len(src)]
            if not pages:
                continue
            label = f"page {pages[0] + 1}" if len(pages) == 1 else f"pages {pages[0] + 1}-{pages[-1] + 1}"
            out = Path(folder) / f"{stem} ({label}).pdf"
            n = 1
            while out.exists():
                n += 1
                out = Path(folder) / f"{stem} ({label}) ({n}).pdf"
            new = pdfium.PdfDocument.new()
            new.import_pages(src, pages=pages)
            new.save(str(out)); new.close()
            outs.append(out)
    finally:
        src.close()
    return outs


def make_smaller(path, output, dpi=110, quality=65, progress=None, cancelled=None):
    """Smaller file by turning every page into a picture. Text can't be selected or searched afterwards."""
    src = _open(path)
    new = pdfium.PdfDocument.new()
    try:
        for i in range(len(src)):
            if cancelled and cancelled():
                raise InterruptedError()
            page = src[i]
            w, h = page.get_size()
            img = page.render(scale=dpi / 72).to_pil().convert("RGB")
            buf = io.BytesIO(); img.save(buf, "JPEG", quality=quality, optimize=True); buf.seek(0)
            out_page = new.new_page(w, h)
            pimg = pdfium.PdfImage.new(new)
            pimg.load_jpeg(buf, inline=True)
            pimg.set_matrix(pdfium.PdfMatrix().scale(w, h))
            out_page.insert_obj(pimg); out_page.gen_content(); out_page.close(); page.close()
            if progress:
                progress((i + 1) / len(src))
        new.save(str(output))
    finally:
        new.close(); src.close()
    return [Path(output)]
