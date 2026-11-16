"""Сводка вариантов снимков для показа: HTML-страница и листы JPG.

python -I tools/photo_compare.py OUT_DIR DEST_DIR
Варианты: out/photos-before-reshoot (первый), out/photos-v2-heavy (с перебором
огрехов), out/photos (текущий, короткий английский промпт).
"""
import glob, html, json, os, shutil, sys
from PIL import Image, ImageDraw, ImageFont

out, dest = sys.argv[1], sys.argv[2]
os.makedirs(os.path.join(dest, "img"), exist_ok=True)
variants = [
    ("photos-before-reshoot", "1. Первый вариант (16.11)"),
    ("photos-v2-heavy", "2. С огрехами, перебор"),
    ("photos", "3. Новый промпт (текущий)"),
]
names = {}
for f in glob.glob(os.path.join(out, "*.jsonl")):
    for line in open(f, encoding="utf-8"):
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if "natural_key" in r:
            names[r["natural_key"].replace(":", "_")] = (r.get("full_name") or r.get("entity_kind", ""), r.get("entity_kind", ""))

def key_of(fn):
    return fn.rsplit("-", 1)[0]

current = sorted(os.path.basename(p) for p in glob.glob(os.path.join(out, "photos", "*.jpg")))
rows = []
for fn in current:
    k = key_of(fn)
    cells = []
    for d, label in variants:
        p = os.path.join(out, d, fn)
        if os.path.exists(p):
            tgt = f"{d}__{fn}"
            shutil.copy(p, os.path.join(dest, "img", tgt))
            cells.append((label, tgt))
    who, kind = names.get(k, (k, ""))
    rows.append((who, kind, fn.rsplit("-", 1)[1][:-4], cells))

font = None
for c in ["C:/Windows/Fonts/arial.ttf", "C:/Windows/Fonts/segoeui.ttf"]:
    if os.path.exists(c):
        font = ImageFont.truetype(c, 22); break

def sheet(items, path, cols=4, h=560):
    ims = []
    for label, p in items:
        im = Image.open(p).convert("RGB")
        im = im.resize((int(im.width * h / im.height), h))
        ims.append((label, im))
    w = max(i.width for _, i in ims)
    rows_n = (len(ims) + cols - 1) // cols
    S = Image.new("RGB", (w * cols, (h + 34) * rows_n), "white")
    d = ImageDraw.Draw(S)
    for n, (label, im) in enumerate(ims):
        x, y = (n % cols) * w, (n // cols) * (h + 34)
        S.paste(im, (x, y + 34))
        d.text((x + 6, y + 4), label, fill="black", font=font)
    S.save(path, quality=88)

# Лист 1: текущие портреты; лист 2: по людям — три варианта рядом; лист 3: дом.
cur = [(f"{r[0]}", os.path.join(dest, "img", f"photos__{f}")) for r, f in zip(rows, current) if r[2] == "portrait"]
if cur:
    sheet(cur, os.path.join(dest, "01_новые_портреты.jpg"))
cmp_items = []
for r in rows:
    if r[2] == "portrait" and len(r[3]) > 1:
        for label, tgt in r[3]:
            cmp_items.append((f"{r[0].split(' ')[0]} · {label.split('.')[0]}", os.path.join(dest, "img", tgt)))
if cmp_items:
    sheet(cmp_items, os.path.join(dest, "02_сравнение_вариантов.jpg"), cols=3, h=480)
house = [(r[2], os.path.join(dest, "img", f"photos__{f}")) for r, f in zip(rows, current) if r[2] != "portrait"]
if house:
    sheet(house, os.path.join(dest, "03_здание.jpg"), cols=3, h=420)

with open(os.path.join(dest, "index.html"), "w", encoding="utf-8") as f:
    f.write("<!doctype html><meta charset=utf-8><title>Mayakramana — снимки</title>"
            "<style>body{font-family:sans-serif;margin:20px}td{vertical-align:top;padding:4px}"
            "img{height:380px}h3{margin:24px 0 4px}</style>"
            "<h1>Mayakramana — варианты снимков</h1>"
            "<p>Все люди и фото сгенерированы. 1 — первый вариант; 2 — пересъёмка с огрехами "
            "(перебор); 3 — текущий: короткий промпт по гайду OpenAI, gpt-image-2.</p>")
    for who, kind, role, cells in rows:
        f.write(f"<h3>{html.escape(who)} <small>{html.escape(kind)} · {role}</small></h3><table><tr>")
        for label, tgt in cells:
            f.write(f"<td><div>{html.escape(label)}</div><img src='img/{tgt}'></td>")
        f.write("</tr></table>")
print(len(rows), "снимков, сводка в", dest)
