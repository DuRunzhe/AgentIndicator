#!/bin/bash
# Simulate upstream format/version changes and see what the tray would report.
set -u
BIN="$1"
W=/tmp/asi-mutation
BASE=$W/base
rm -rf "$W"; mkdir -p "$BASE/storages/session_projcache/sessions" "$BASE/sessions/--Users-me-code-app--/session-m1"

make_doc() { # $1 = python expression for the mutation
  python3 - "$1" <<PY
import json, sys, time
mut = sys.argv[1]
rows = {
  "sessionStats": {"val": {"openStep": None, "pendingCalls": {}}},
  "userQuestions": {"val": {"questions": {"active": []}}},
  "inbox": {"val": {"next-turn": [], "next-step": []}},
  "title": {"val": "变异测试会话"},
  "modelSelection": {"val": {"lastUsed": {"model": "deepseek-flash"}}},
  "contextPressure": {"val": {"pressureTokens": 1000, "contextWindow": 1000000}},
  "sessionListMetadata": {"val": {"blank": False, "lastPromptAt": int(time.time()*1000)}},
  "permissions": {"val": {"approval": "ask"}},
}
doc = {"version": 7, "record": {"identity": {"createdAt": 1791531000000, "cwd": "/Users/me/code/app"}, "rows": rows}}
exec(mut)
open("$BASE/storages/session_projcache/sessions/session-m1.json", "w").write(json.dumps(doc))
PY
}

# the offline failure log, shared by every mutation
make_log() {
  printf '{"type":"turn/start","data":{"turn":1}}\n{"type":"step/start","data":{"turn":1,"step":1}}\n%s\n{"type":"step/end","data":{"turn":1,"step":1}}\n{"type":"turn/end","data":{"turn":1}}\n' "$1" \
    | zstd -q -f -o "$BASE/sessions/--Users-me-code-app--/session-m1/session.v4.jsonl.zstd"
}

report() {
  local label="$1"
  local out
  out=$(DSH_HOME=$BASE "$BIN" --diagnose-deepseek-desktop 2>/dev/null)
  python3 - "$label" <<PY
import json, sys
label = sys.argv[1]
d = json.loads('''$out''')
h = d.get('health', {})
sessions = d.get('sessions', [])
state = sessions[0]['state'] if sessions else '(无会话)'
sig = sessions[0]['signals'] if sessions else {}
print(f"  {label:38} state={state:12} intact={h.get('intact')} verMis={h.get('versionMismatch')} unreadable={h.get('unreadable')} incomplete={h.get('incomplete')} unjudged={h.get('retryUnjudged')} unknown={sig.get('unknownEvents')}")
PY
}

EXHAUSTED='{"type":"llm/retry","data":{"turn":1,"step":1,"mode":"normal","retry":5,"maxRetries":5,"failure":{"message":"x","code":"TRANSPORT"}}}'

echo "=== 基线（当前格式，离线失败应报 error）==="
make_doc "pass"; make_log "$EXHAUSTED"; report "基线"

echo
echo "=== 变异 1: 投影 version 7 -> 8（上游改版本号）==="
make_doc "doc['version']=8"; report "version=8"

echo "=== 变异 2: sessionStats 改名（rows 结构变动）==="
make_doc "doc['record']['rows']['turnStats']=doc['record']['rows'].pop('sessionStats')"; report "sessionStats 改名"

echo "=== 变异 3: 事件类型改名 llm/retry -> llm/retrying ==="
make_log "$(echo "$EXHAUSTED" | sed 's|llm/retry|llm/retrying|')"; report "llm/retry 改名"

echo "=== 变异 4: maxRetries 字段改名（预算不可判）==="
make_log '{"type":"llm/retry","data":{"turn":1,"step":1,"mode":"normal","retry":5,"retryBudget":5}}'; report "maxRetries 改名"

echo "=== 变异 5: lastPromptAt 消失（活动时间不可知）==="
make_doc "del doc['record']['rows']['sessionListMetadata']['val']['lastPromptAt']"; report "lastPromptAt 消失"

echo "=== 变异 6: 存储目录改名（整个布局变动）==="
mv "$BASE/storages/session_projcache" "$BASE/storages/session_cache" 2>/dev/null; report "存储目录改名"
mv "$BASE/storages/session_cache" "$BASE/storages/session_projcache" 2>/dev/null
