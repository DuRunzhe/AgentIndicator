#!/bin/bash
# Deterministic check: does the packaged binary report error for an exhausted retry?
set -u
BIN="$1"

# The diagnostic prefers the profile of a *running* desktop host over DSH_HOME,
# so a live app would mask this fixture. Close it, or expect empty output.
if /bin/ps -axo command= 2>/dev/null | grep -q "DeepSeek Harness.app/Contents/MacOS"; then
  echo "警告: 检测到桌面端正在运行，诊断会优先读取它的 profile，本脚本的固定数据可能被忽略。" >&2
fi
W=/tmp/asi-error-verify
rm -rf "$W"; mkdir -p "$W/storages/session_projcache/sessions" "$W/sessions/--Users-me-code-app--/session-err"

python3 - <<PY
import json, time
rows = {
  "sessionStats": {"val": {"openStep": None, "pendingCalls": {}}},
  "userQuestions": {"val": {"questions": {"active": []}}},
  "inbox": {"val": {"next-turn": [], "next-step": []}},
  "title": {"val": "失败测试"},
  "modelSelection": {"val": {"lastUsed": {"model": "deepseek-flash"}}},
  "contextPressure": {"val": {"pressureTokens": 1000, "contextWindow": 1000000}},
  "sessionListMetadata": {"val": {"blank": False, "lastPromptAt": int(__import__("time").time()*1000)}},
  "permissions": {"val": {"approval": "ask"}},
}
doc = {"version": 7, "record": {"identity": {"createdAt": 1791531000000, "cwd": "/Users/me/code/app"}, "rows": rows}}
open("$W/storages/session_projcache/sessions/session-err.json", "w").write(json.dumps(doc))
PY

run() { DSH_HOME=$W "$BIN" --diagnose-deepseek-desktop 2>/dev/null | python3 -c "
import json,sys
d=json.load(sys.stdin)
for s in d['sessions']:
    if 'session-err' in s['id']:
        print('   state=%-8s signals=%s' % (s['state'], json.dumps(s['signals'])))
"; }

echo "--- 场景 A: 调用数 2/5（仍在预算内，只应视为工作/就绪）---"
printf '{"type":"turn/start","data":{"turn":1}}\n{"type":"step/start","data":{"turn":1,"step":1}}\n{"type":"llm/retry","data":{"turn":1,"step":1,"mode":"normal","retry":2,"maxRetries":5,"failure":{"message":"boom"}}}\n' | zstd -q -f -o "$W/sessions/--Users-me-code-app--/session-err/session.v4.jsonl.zstd"
run

echo "--- 场景 B: 调用数 5/5（预算耗尽，应判异常）---"
printf '{"type":"turn/start","data":{"turn":1}}\n{"type":"step/start","data":{"turn":1,"step":1}}\n{"type":"llm/retry","data":{"turn":1,"step":1,"mode":"normal","retry":5,"maxRetries":5,"failure":{"message":"boom"}}}\n' | zstd -q -f -o "$W/sessions/--Users-me-code-app--/session-err/session.v4.jsonl.zstd"
run

echo "--- 场景 C: 失败后该轮关闭且有恢复进展（应回到就绪）---"
printf '{"type":"turn/start","data":{"turn":1}}\n{"type":"step/start","data":{"turn":1,"step":1}}\n{"type":"llm/retry","data":{"turn":1,"step":1,"mode":"normal","retry":5,"maxRetries":5,"failure":{"message":"boom"}}}\n{"type":"step/end","data":{"turn":1,"step":1}}\n{"type":"turn/end","data":{"turn":1}}\n{"type":"turn/start","data":{"turn":2}}\n{"type":"assistant/message","data":{"turn":2,"step":1}}\n' | zstd -q -f -o "$W/sessions/--Users-me-code-app--/session-err/session.v4.jsonl.zstd"
run

echo "--- 场景 D: 离线形态（重试耗尽→该轮关闭→其后无任何进展，应判异常）---"
printf '{"type":"turn/start","data":{"turn":1}}\n{"type":"step/start","data":{"turn":1,"step":1}}\n{"type":"llm/retry","data":{"turn":1,"step":1,"mode":"normal","retry":5,"maxRetries":5,"failure":{"message":"transport failed","code":"TRANSPORT"}}}\n{"type":"step/end","data":{"turn":1,"step":1}}\n{"type":"turn/end","data":{"turn":1}}\n' | zstd -q -f -o "$W/sessions/--Users-me-code-app--/session-err/session.v4.jsonl.zstd"
run
