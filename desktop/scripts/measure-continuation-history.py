"""Join durable tool terminals to actual local-provider HTTP arrival times."""
import json
from pathlib import Path
import sqlite3
import sys

root = Path(sys.argv[1]).resolve()
requests = [r for r in json.loads((root/'requests.json').read_text()) if r['mode'] == 'measure']
db = root/'runtime/history/aworkit.sqlite3'
connection = sqlite3.connect(db.as_uri()+'?mode=ro', uri=True)
rows = connection.execute("SELECT payload FROM semantic_events WHERE kind='span.completed' AND json_extract(payload,'$.spanId') LIKE 'span.tool.performance.call.%' ORDER BY sequence")
terminals = {t['spanId']: t for r in rows if (t := json.loads(r[0]))}
measurements = []
for request in requests:
    span = 'span.tool.' + request.get('completedCall', '')
    if span in terminals:
        measurements.append({'call': span, 'delayMs': request['arrivedAt']-int(terminals[span]['createdAt'])})
print(json.dumps(measurements))
