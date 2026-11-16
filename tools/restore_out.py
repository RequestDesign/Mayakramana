"""Восстановить каталог результатов (out/) из базы прогонов.

Записи прогонов лежат в synthforge.db (jobs.result); out/*.jsonl — их копия,
из которой собираются центры и делаются снимки. Если out/ потерян, его можно
собрать заново: python -I tools/restore_out.py [synthforge.db] [out]
Существующие файлы не перезаписываются.
"""
import json, os, sqlite3, sys

db = sys.argv[1] if len(sys.argv) > 1 else "synthforge.db"
out = sys.argv[2] if len(sys.argv) > 2 else "out"
os.makedirs(out, exist_ok=True)
c = sqlite3.connect(f"file:{db}?mode=ro", uri=True)
rows = c.execute(
    "select s.id, s.kind, j.natural_key, j.result, j.batch_id from jobs j "
    "join sessions s on s.id = j.session_id "
    "where j.status = 'done' and j.result is not null order by s.created_at, j.id"
).fetchall()
by = {}
for sid, kind, key, result, batch in rows:
    try:
        r = json.loads(result)
    except ValueError:
        continue
    r["session_id"], r["natural_key"] = sid, key
    if batch:
        r["batch_id"] = batch
    by.setdefault(kind + "s", []).append(r)
for coll, recs in by.items():
    path = os.path.join(out, coll + ".jsonl")
    if os.path.exists(path):
        print(f"{path}: уже есть, пропущен")
        continue
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        for r in recs:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"{path}: {len(recs)}")
