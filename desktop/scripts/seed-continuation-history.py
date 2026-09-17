"""Populate only the stopped, isolated native performance profile.

Repeated historical snapshots reproduce the incident's byte volume, while the
latest snapshot has a 2 MiB active context. No saved invocation is dispatched.
"""
import json
import copy
from pathlib import Path
import sqlite3
import sys
import time

root = Path(sys.argv[1]).resolve()
if not root.name.startswith('native-continuation-') or 'target' not in root.parts:
    raise SystemExit('Expected an isolated native-continuation target profile')
db = root / 'runtime/history/aworkit.sqlite3'
with sqlite3.connect(db) as connection:
    chat, branch, head = connection.execute(
        "SELECT chat_id,branch_id,head_sequence FROM chat_streams WHERE chat_id LIKE 'chat.%' "
        "AND chat_id != 'chat.frozen-sessions' ORDER BY head_sequence DESC LIMIT 1"
    ).fetchone()
    snapshot = json.loads(connection.execute(
        "SELECT payload FROM semantic_events WHERE chat_id=? AND kind='context.checkpoint' "
        "ORDER BY sequence DESC LIMIT 1", (chat,)
    ).fetchone()[0])
    document = snapshot['snapshot']['document']
    exchange_template = document['exchanges'][0]
    exchanges = []
    for n in range(450):
        exchange = copy.deepcopy(exchange_template)
        call = next(part['call'] for part in exchange['assistantContent'] if part['kind'] == 'tool_call')
        call['callId'] = call['providerCallId'] = f'historical.call.{n}'
        exchange['results'][0]['callId'] = call['callId']
        exchange['results'][0]['content'] = {'historicalEvidence': 'implementation output\n'*195}
        exchanges.append(exchange)
    document['exchanges'] = exchanges
    snapshot['snapshot']['anchor'] = None
    snapshot['createdAt'] = str(int(time.time() * 1000))
    raw = json.dumps(snapshot, separators=(',', ':'))
    count = int(sys.argv[2]) if len(sys.argv) > 2 else 640
    connection.executemany(
        'INSERT INTO semantic_events(event_id,chat_id,branch_id,sequence,kind,payload) VALUES(?,?,?,?,?,?)',
        ((f'performance.snapshot.{n}', chat, branch, head+n+1, 'context.checkpoint', raw) for n in range(count))
    )
    metadata_count = 13500
    connection.executemany(
        'INSERT INTO semantic_events(event_id,chat_id,branch_id,sequence,kind,payload) VALUES(?,?,?,?,?,?)',
        ((f'performance.metadata.{n}', chat, branch, head+count+n+1, 'context.performance-fixture',
          json.dumps({'ordinal':n,'requestId':'performance.historical'})) for n in range(metadata_count))
    )
    connection.execute('UPDATE chat_streams SET head_sequence=?,aggregate_version=? WHERE chat_id=? AND branch_id=?',
                       (head+count+metadata_count, head+count+metadata_count, chat, branch))
    connection.commit()
    metrics = {'chatId': chat, 'historicalPayloadBytes': len(raw.encode())*count,
               'activeContextBytes': len(raw.encode()), 'snapshots': count, 'historicalEvents': head+count+metadata_count}

# Unrelated historical tool outputs exercise the operational-record cache too.
with sqlite3.connect(root/'runtime/history/aworkit-invocations.sqlite3') as connection:
    chat, branch, raw = connection.execute("SELECT chat_id,branch_id,payload FROM semantic_events WHERE kind='pipeline.tool-outcome' LIMIT 1").fetchone()
    head = connection.execute('SELECT head_sequence FROM chat_streams WHERE chat_id=? AND branch_id=?', (chat,branch)).fetchone()[0]
    template = json.loads(raw)
    values = []
    for n in range(1536):
        template['record']['invocationId'] = f'performance.historical.invocation.{n}'
        template['record']['result'] = {'historicalOutput': 'x'*65536}
        values.append((f'performance.historical.outcome.{n}',chat,branch,head+n+1,'pipeline.tool-outcome',json.dumps(template,separators=(',',':'))))
    connection.executemany('INSERT INTO semantic_events(event_id,chat_id,branch_id,sequence,kind,payload) VALUES(?,?,?,?,?,?)',values)
    connection.execute('UPDATE chat_streams SET head_sequence=?,aggregate_version=? WHERE chat_id=? AND branch_id=?',(head+len(values),head+len(values),chat,branch))
    metrics['operationalPayloadBytes'] = sum(len(v[-1].encode()) for v in values)
print(json.dumps(metrics))
